import 'dart:ui';

import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/selector/maestro_regex.dart';
import 'package:tapwing_driver/src/selector/selector.dart';
import 'package:tapwing_driver/src/snapshot/snapshot.dart';

int _nextId = 1;

KNode _node({
  KNode? parent,
  Rect bounds = const Rect.fromLTWH(0, 0, 100, 50),
  String? text,
  String? hintText,
  String? accessibilityText,
  String resourceId = '',
  bool clickable = false,
  bool enabled = true,
  bool focused = false,
  bool checked = false,
  bool selected = false,
  bool isTextField = false,
  bool scrollable = false,
  String? error,
}) {
  final KNode n = KNode(
    id: _nextId++,
    parent: parent,
    bounds: bounds,
    text: text,
    hintText: hintText,
    accessibilityText: accessibilityText,
    resourceId: resourceId,
    clickable: clickable,
    enabled: enabled,
    focused: focused,
    checked: checked,
    selected: selected,
    isTextField: isTextField,
    scrollable: scrollable,
    error: error,
  );
  parent?.children.add(n);
  return n;
}

Snapshot _snap(KNode root, {Size screen = const Size(1000, 2000)}) {
  final List<KNode> nodes = <KNode>[];
  void visit(KNode n) {
    nodes.add(n);
    for (final KNode c in n.children) {
      visit(c);
    }
  }

  visit(root);
  return Snapshot(root, nodes, screen, 0);
}

void main() {
  group('Selector matching (Maestro 2.12.1 semantics)', () {
    test('text regex full-match, ignore case, dotAll, newline-to-space, invalid regex fallback', () {
      final KNode root = _node(bounds: const Rect.fromLTWH(0, 0, 1000, 2000));
      final KNode tile = _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 100, 1000, 100),
        accessibilityText: 'Product 1\n€ 3.00',
      );
      final KNode weird = _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 220, 400, 60),
        text: 'a)(b',
      );
      final KNode hint = _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 300, 400, 60),
        text: '',
        hintText: 'Email Address',
        isTextField: true,
      );
      final Snapshot snap = _snap(root);

      expect(resolve(Selector(text: 'Product 1'), snap), isNull);
      expect(resolve(Selector(text: 'product 1.*'), snap), same(tile));
      expect(resolve(Selector(text: 'Product 1 € 3.00'), snap), same(tile));
      expect(resolve(Selector(text: 'email address'), snap), same(hint));
      // Unbalanced parens must fall back to literal matching (toRegexSafe).
      expect(resolve(Selector(text: 'a)(b'), snap), same(weird));
      expect(resolve(Selector(text: 'a)(b'), _snap(_node(text: 'ab'))), isNull);
    });

    test('id matching: empty resourceId matches like in Maestro, suffix after slash', () {
      final KNode root = _node(bounds: const Rect.fromLTWH(0, 0, 1000, 2000));
      final KNode noId = _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 0, 200, 50),
        text: 'No ID',
      );
      final KNode withId = _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 60, 200, 50),
        resourceId: 'dev.tapwing:id/login_button',
      );
      final Snapshot snap = _snap(root);

      // Root matches too but has matching descendants (deepestMatchingElement).
      expect(resolveAll(Selector(id: '.*'), snap), <KNode>[noId, withId]);
      expect(resolveAll(Selector(id: '.+'), snap), <KNode>[withId]);
      expect(resolve(Selector(id: 'login_button'), snap), same(withId));
      expect(
        resolve(Selector(id: 'dev.tapwing:id/login_button'), snap),
        same(withId),
      );
    });

    test('deepestMatchingElement keeps only deepest matching nodes', () {
      final KNode root = _node(
        bounds: const Rect.fromLTWH(0, 0, 1000, 2000),
        enabled: true,
      );
      final KNode wrapper = _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 100, 500, 200),
        accessibilityText: 'Submit',
        enabled: true,
      );
      final KNode leaf = _node(
        parent: wrapper,
        bounds: const Rect.fromLTWH(10, 110, 100, 50),
        accessibilityText: 'Submit',
        enabled: true,
      );
      final KNode sibling = _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 350, 100, 50),
        accessibilityText: 'Cancel',
        enabled: true,
      );
      final Snapshot snap = _snap(root);

      expect(resolveAll(Selector(text: 'Submit'), snap), <KNode>[leaf]);
      expect(resolveAll(Selector(enabled: true), snap), <KNode>[leaf, sibling]);
    });

    test('index sorts by y then x and supports negative indices; clickableFirst otherwise', () {
      final KNode root = _node(bounds: const Rect.fromLTWH(0, 0, 1000, 2000));
      final KNode bottomRight = _node(
        parent: root,
        bounds: const Rect.fromLTWH(300, 400, 100, 50),
        text: 'Item',
        clickable: false,
      );
      final KNode topRight = _node(
        parent: root,
        bounds: const Rect.fromLTWH(300, 100, 100, 50),
        text: 'Item',
        clickable: true,
      );
      final KNode topLeft = _node(
        parent: root,
        bounds: const Rect.fromLTWH(50, 100, 100, 50),
        text: 'Item',
        clickable: false,
      );
      final Snapshot snap = _snap(root);

      // Without index: clickableFirst picks topRight over bottomRight & topLeft.
      expect(resolve(Selector(text: 'Item'), snap), same(topRight));
      // With index: sorted by (top, left) -> [topLeft, topRight, bottomRight].
      expect(resolve(Selector(text: 'Item', index: 0), snap), same(topLeft));
      expect(resolve(Selector(text: 'Item', index: 1), snap), same(topRight));
      expect(
        resolve(Selector(text: 'Item', index: -1), snap),
        same(bottomRight),
      );
      expect(resolve(Selector(text: 'Item', index: 5), snap), isNull);
      expect(resolve(Selector(text: 'Item', index: -4), snap), isNull);
    });

    test('inBounds drops <10% visible nodes, keeps overflowing fullscreen containers', () {
      final KNode root = _node(bounds: const Rect.fromLTWH(0, 0, 1000, 2000));
      final KNode offscreen = _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 1995, 200, 100), // 5% visible
        text: 'Offscreen',
      );
      final KNode overflowing = _node(
        parent: root,
        bounds: const Rect.fromLTWH(
          -50,
          -5000,
          1100,
          25000,
        ), // covers whole screen, area ratio < 10%
        text: 'ScrollContainer',
      );
      final Snapshot snap = _snap(root);

      expect(resolve(Selector(text: offscreen.text), snap), isNull);
      expect(
        resolve(Selector(text: 'ScrollContainer'), snap),
        same(overflowing),
      );
    });

    test('relative selectors: below, childOf, containsChild, containsDescendants (in-bounds only)', () {
      final KNode root = _node(bounds: const Rect.fromLTWH(0, 0, 1000, 2000));
      _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 50, 500, 50),
        text: 'Header',
      );
      final KNode cardA = _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 150, 500, 200),
        resourceId: 'card',
      );
      final KNode btnA = _node(
        parent: cardA,
        bounds: const Rect.fromLTWH(20, 200, 100, 50),
        text: 'Buy',
        clickable: true,
      );
      final KNode cardB = _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 400, 500, 200),
        resourceId: 'card',
      );
      _node(
        parent: cardB,
        bounds: const Rect.fromLTWH(20, 2100, 100, 50), // completely offscreen
        text: 'Ghost',
      );
      final Snapshot snap = _snap(root);

      expect(
        resolve(
          Selector(
            text: 'Buy',
            below: Selector(text: 'Header'),
          ),
          snap,
        ),
        same(btnA),
      );
      expect(
        resolve(
          Selector(
            text: 'Buy',
            above: Selector(text: 'Header'),
          ),
          snap,
        ),
        isNull,
      );
      expect(
        resolve(
          Selector(
            text: 'Buy',
            childOf: Selector(id: 'card', index: 0),
          ),
          snap,
        ),
        same(btnA),
      );
      expect(
        resolveAll(
          Selector(
            text: 'Buy',
            childOf: Selector(id: 'card', index: 1),
          ),
          snap,
        ),
        isEmpty,
      );
      expect(
        resolve(
          Selector(
            id: 'card',
            containsChild: Selector(text: 'Buy'),
          ),
          snap,
        ),
        same(cardA),
      );
      expect(
        resolve(
          Selector(
            id: 'card',
            containsDescendants: <Selector>[Selector(text: 'Buy')],
          ),
          snap,
        ),
        same(cardA),
      );
      // Offscreen descendant must NOT match containsDescendants (Maestro filterOutOfBounds).
      expect(
        resolve(
          Selector(
            id: 'card',
            containsDescendants: <Selector>[Selector(text: 'Ghost')],
          ),
          snap,
        ),
        isNull,
      );
    });

    test('leftOf, rightOf, size with tolerance, and state flags', () {
      final KNode root = _node(bounds: const Rect.fromLTWH(0, 0, 1000, 2000));
      final KNode left = _node(
        parent: root,
        bounds: const Rect.fromLTWH(50, 200, 120, 48),
        text: 'Cancel',
        enabled: false,
      );
      final KNode right = _node(
        parent: root,
        bounds: const Rect.fromLTWH(300, 200, 122, 50),
        text: 'Save',
        checked: true,
        focused: true,
        selected: true,
      );
      final Snapshot snap = _snap(root);

      expect(
        resolve(
          Selector(
            text: 'Cancel',
            leftOf: Selector(text: 'Save'),
          ),
          snap,
        ),
        same(left),
      );
      expect(
        resolve(
          Selector(
            text: 'Save',
            rightOf: Selector(text: 'Cancel'),
          ),
          snap,
        ),
        same(right),
      );
      expect(
        resolve(Selector(width: 120, height: 50, tolerance: 2), snap),
        same(left),
      );
      expect(
        resolve(Selector(width: 120, height: 50, tolerance: 0), snap),
        isNull,
      );
      expect(resolve(Selector(enabled: false), snap), same(left));
      expect(
        resolve(Selector(checked: true, focused: true, selected: true), snap),
        same(right),
      );
    });

    test('Selector.fromJson, describe, and KNode helpers', () {
      final Selector sel = Selector.fromJson(<String, Object?>{
        'text': 'Buy',
        'id': 'btn_buy',
        'index': -1,
        'enabled': true,
        'checked': false,
        'focused': true,
        'selected': false,
        'width': 100,
        'height': 50,
        'tolerance': 4,
        'below': <String, Object?>{'text': 'Title'},
        'containsDescendants': <Object?>[
          <String, Object?>{'text': 'Price'},
        ],
      });

      expect(sel.text, 'Buy');
      expect(sel.id, 'btn_buy');
      expect(sel.index, -1);
      expect(sel.below?.text, 'Title');
      expect(sel.containsDescendants?.single.text, 'Price');
      expect(sel.describe(), contains('Text matching regex: Buy'));
      expect(sel.describe(), contains('Id matching regex: btn_buy'));
      expect(sel.describe(), contains('Below: Text matching regex: Title'));
      expect(sel.describe(), contains('Index: -1'));
      expect(sel.describe(), contains('Enabled'));
      expect(sel.describe(), contains('Not checked'));

      final KNode parent = _node(bounds: const Rect.fromLTWH(0, 0, 200, 200));
      final KNode child = _node(
        parent: parent,
        bounds: const Rect.fromLTWH(10, 20, 80, 40),
        text: 'Hello',
        resourceId: 'greet',
        clickable: true,
        enabled: false,
      );
      expect(child.isDescendantOf(parent), isTrue);
      expect(parent.isDescendantOf(child), isFalse);
      final Map<String, Object?> json = parent.toJson(withChildren: true);
      expect((json['children']! as List<Object?>).length, 1);
      final Map<String, Object?> childJson =
          (json['children']! as List<Object?>).first! as Map<String, Object?>;
      expect(childJson['resource-id'], 'greet');
      expect(childJson['bounds'], <int>[10, 20, 90, 60]);
      expect(childJson['clickable'], true);
      expect(childJson['enabled'], false);
    });

    test('MaestroRegex handles invalid brackets and literal equality', () {
      final MaestroRegex broken = MaestroRegex('[unclosed');
      expect(broken.fullMatch('[unclosed'), isTrue);
      expect(broken.fullMatch('unclosed'), isFalse);

      final MaestroRegex literal = MaestroRegex(r'$5.00');
      expect(literal.matchesText(r'$5.00'), isTrue);
    });

    test(
      'traits: text, square (<3% aspect tolerance), and long-text (>200 chars)',
      () {
        final KNode root = _node(bounds: const Rect.fromLTWH(0, 0, 1000, 2000));
        final KNode noTextRect = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 0, 200, 100),
        );
        final KNode shortText = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 100, 200, 100),
          text: 'Hello',
        );
        final KNode exact200 = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 200, 200, 100),
          text: 'a' * 200,
        );
        final KNode longText = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 300, 200, 100),
          text: 'b' * 201,
        );
        final KNode exactSquare = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 400, 100, 100),
        );
        final KNode almostSquare = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 500, 102, 100), // 2% diff < 3%
        );
        final KNode notSquare = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 600, 104, 100), // 4% diff >= 3%
        );
        final KNode zeroHeight = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 700, 100, 0),
        );
        final Snapshot snap = _snap(root);

        expect(
          resolveAll(Selector(traits: <ElementTrait>[ElementTrait.text]), snap),
          <KNode>[shortText, exact200, longText],
        );
        expect(
          resolveAll(
            Selector(traits: <ElementTrait>[ElementTrait.longText]),
            snap,
          ),
          <KNode>[longText],
        );
        expect(
          resolveAll(
            Selector(traits: <ElementTrait>[ElementTrait.square]),
            snap,
          ),
          <KNode>[exactSquare, almostSquare],
        );
        expect(
          resolve(
            Selector(
              traits: <ElementTrait>[ElementTrait.square],
              text: 'Hello',
            ),
            snap,
          ),
          isNull,
        );
        // Unused local refs kept explicitly to clarify tree composition.
        expect(noTextRect.text, isNull);
        expect(notSquare.bounds.width, 104);
        expect(zeroHeight.bounds.height, 0);

        // Selector.fromJson + describe for traits and css.
        final Selector parsed = Selector.fromJson(<String, Object?>{
          'traits': <String>['TEXT', 'SQUARE', 'LONG_TEXT'],
          'css': '#submit',
        });
        expect(parsed.traits, <ElementTrait>[
          ElementTrait.text,
          ElementTrait.square,
          ElementTrait.longText,
        ]);
        expect(parsed.describe(), contains('Has text'));
        expect(parsed.describe(), contains('Is square'));
        expect(parsed.describe(), contains('Has long text'));
        expect(parsed.describe(), contains('CSS: #submit'));
      },
    );

    test('css selector matches nothing on Android (Driver.kt:122-124, Filters.kt:299-316)', () {
      final KNode root = _node(bounds: const Rect.fromLTWH(0, 0, 1000, 2000));
      _node(
        parent: root,
        bounds: const Rect.fromLTWH(0, 0, 200, 60),
        text: 'Submit',
        resourceId: 'submit',
      );
      final Snapshot snap = _snap(root);

      expect(resolveAll(Selector(css: '#submit'), snap), isEmpty);
      expect(
        resolveAll(Selector(text: 'Submit', css: '#submit'), snap),
        isEmpty,
      );
    });

    // Ported from Maestro FiltersTest.kt
    test(
      'FiltersTest port: error attribute matching and blank error guard',
      () {
        final KNode root = _node(bounds: const Rect.fromLTWH(0, 0, 1000, 2000));
        final KNode errNode = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 10, 200, 50),
          text: '',
          hintText: '',
          accessibilityText: '',
          error: 'This is an error!',
        );
        final KNode blankErrOnly = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 70, 200, 50),
          error: '',
        );
        final KNode textNode = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 130, 200, 50),
          text: 'Hello',
        );
        final KNode hintNode = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 190, 200, 50),
          hintText: 'Hello',
        );
        final KNode a11yNode = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 250, 200, 50),
          accessibilityText: 'Hello',
        );
        final KNode errHello = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 310, 200, 50),
          text: '',
          error: 'Hello',
        );
        final Snapshot snap = _snap(root);

        expect(
          resolve(Selector(text: 'This is an error!'), snap),
          same(errNode),
        );
        expect(resolve(Selector(text: 'This is an.*'), snap), same(errNode));
        expect(resolve(Selector(text: 'Some other text'), snap), isNull);
        expect(
          resolveAll(Selector(text: 'Hello'), snap),
          containsAll(<KNode>[textNode, hintNode, a11yNode, errHello]),
        );
        // Blank error alone must not match empty regex.
        expect(resolveAll(Selector(text: ''), _snap(blankErrOnly)), isEmpty);
      },
    );

    // Ported from Maestro FiltersTest.kt (index bounds)
    test(
      'FiltersTest port: index positive, negative, -size, and out-of-bounds',
      () {
        final KNode root = _node(bounds: const Rect.fromLTWH(0, 0, 1000, 2000));
        final KNode n0 = _node(
          parent: root,
          bounds: const Rect.fromLTWH(0, 0, 5, 5),
          text: 'x',
        );
        final KNode n1 = _node(
          parent: root,
          bounds: const Rect.fromLTWH(10, 10, 5, 5),
          text: 'x',
        );
        final KNode n2 = _node(
          parent: root,
          bounds: const Rect.fromLTWH(20, 20, 5, 5),
          text: 'x',
        );
        final Snapshot snap = _snap(root);

        expect(resolveAll(Selector(text: 'x', index: 1), snap), <KNode>[n1]);
        expect(resolveAll(Selector(text: 'x', index: -1), snap), <KNode>[n2]);
        expect(resolveAll(Selector(text: 'x', index: -3), snap), <KNode>[n0]);
        expect(resolveAll(Selector(text: 'x', index: -4), snap), isEmpty);
      },
    );

    // Ported from Maestro UiElementTest.kt
    test('UiElementTest port: visibleFraction exact percentages', () {
      const double w = 720;
      const double h = 1280;

      final KNode full = _node(bounds: const Rect.fromLTWH(50, 50, 200, 100));
      expect(visibleFraction(full, w, h), 1.0);

      final KNode leftBottom = _node(
        bounds: const Rect.fromLTWH(-50, 1260, 200, 100),
      );
      expect(visibleFraction(leftBottom, w, h), closeTo(0.15, 1e-9));

      final KNode rightBottom = _node(
        bounds: const Rect.fromLTWH(680, 1200, 200, 100),
      );
      expect(visibleFraction(rightBottom, w, h), closeTo(0.16, 1e-9));

      final KNode outOfBounds = _node(
        bounds: const Rect.fromLTWH(-200, 1300, 200, 100),
      );
      expect(visibleFraction(outOfBounds, w, h), 0.0);
    });

    test('isElementNearScreenCenter matches UiElement.kt:49-68 for all swipe directions', () {
      const double w = 1000;
      const double h = 2000;
      // center = (500, 1000), marginY = 400, marginX = 200
      final KNode mid = _node(bounds: const Rect.fromLTWH(450, 950, 100, 100));
      final KNode nearBottom = _node(
        bounds: const Rect.fromLTWH(
          450,
          1500,
          100,
          100,
        ), // centerY = 1550 > 1400
      );
      final KNode nearTop = _node(
        bounds: const Rect.fromLTWH(450, 400, 100, 100), // centerY = 450 < 600
      );
      final KNode nearRight = _node(
        bounds: const Rect.fromLTWH(750, 950, 100, 100), // centerX = 800 > 700
      );
      final KNode nearLeft = _node(
        bounds: const Rect.fromLTWH(150, 950, 100, 100), // centerX = 200 < 300
      );

      expect(isElementNearScreenCenter(mid, 'UP', w, h), isTrue);
      expect(isElementNearScreenCenter(nearBottom, 'UP', w, h), isFalse);
      expect(isElementNearScreenCenter(mid, 'DOWN', w, h), isTrue);
      expect(isElementNearScreenCenter(nearTop, 'DOWN', w, h), isFalse);
      expect(isElementNearScreenCenter(mid, 'LEFT', w, h), isTrue);
      expect(isElementNearScreenCenter(nearRight, 'LEFT', w, h), isFalse);
      expect(isElementNearScreenCenter(mid, 'RIGHT', w, h), isTrue);
      expect(isElementNearScreenCenter(nearLeft, 'RIGHT', w, h), isFalse);
    });
  });
}
