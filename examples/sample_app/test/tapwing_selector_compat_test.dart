// The Maestro flows in `.maestro/` select elements through `tapwing_driver`'s
// selector engine. These tests run that same engine against the real sample
// app so a UI change that would silently break a flow fails here first.
//
// ignore_for_file: implementation_imports
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/selector/selector.dart';
import 'package:tapwing_driver/src/snapshot/snapshot.dart';
import 'package:sample_app/main.dart';

/// `testWidgets` with the semantics tree enabled (what the driver reads) for
/// the whole test, disposed before the framework's end-of-test checks.
void _testApp(String description, Future<void> Function(WidgetTester) body) {
  testWidgets(description, (WidgetTester tester) async {
    final SemanticsHandle handle = tester.ensureSemantics();
    try {
      await body(tester);
    } finally {
      handle.dispose();
    }
  });
}

/// Pumps the app on a 360x1400 logical (1080x4200 physical, 3x) surface, so
/// `bounds` are in the physical pixels the driver reports on Android.
Future<void> _pump(WidgetTester tester) async {
  tester.view
    ..physicalSize = const Size(1080, 4200)
    ..devicePixelRatio = 3;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(const SampleApp());
  await tester.pumpAndSettle();
}

KNode? _find(Selector selector) => resolve(selector, Snapshot.capture());

KNode _expectFound(Selector selector) {
  final KNode? node = _find(selector);
  expect(node, isNotNull, reason: 'no element for ${selector.describe()}');
  return node!;
}

Future<void> _openPlayground(WidgetTester tester) async {
  await tester.tap(find.text('F2 Playground'));
  await tester.pumpAndSettle();
}

void main() {
  group('Login screen selectors used by login_and_browse.yaml', () {
    _testApp('text selectors find the title and the button', (
      WidgetTester tester,
    ) async {
      await _pump(tester);

      expect(
        _expectFound(Selector(text: 'Welcome back')).accessibilityText,
        'Welcome back',
      );
      final KNode login = _expectFound(Selector(text: 'Log in'));
      expect(login.clickable, isTrue);
      expect(login.enabled, isTrue);
      // Maestro text selectors are case-insensitive full-match regexes.
      _expectFound(Selector(text: 'log IN'));
      _expectFound(Selector(text: 'Welcome.*'));
      expect(
        _find(Selector(text: 'Welcome')),
        isNull,
        reason: 'full match only',
      );
    });

    _testApp('id selectors resolve the Semantics identifiers', (
      WidgetTester tester,
    ) async {
      await _pump(tester);

      for (final String id in <String>[
        'email_input',
        'password_input',
        'login_button',
        'playground_button',
      ]) {
        expect(_expectFound(Selector(id: id)).resourceId, id);
      }
      expect(_find(Selector(id: 'no_such_id')), isNull);
    });

    _testApp('text fields expose their label as hintText and start empty', (
      WidgetTester tester,
    ) async {
      await _pump(tester);

      final KNode email = _expectFound(Selector(id: 'email_input'));
      expect(email.isTextField, isTrue);
      expect(email.text, isEmpty);
      expect(email.hintText, 'Email');
      // `tapOn: "Email"` in the flow reaches the field through its hint text.
      final KNode byLabel = _expectFound(Selector(text: 'Email'));
      expect(byLabel.isTextField, isTrue);
      expect(byLabel.bounds, email.bounds);
      expect(_expectFound(Selector(id: 'password_input')).hintText, 'Password');
    });

    _testApp('a typed value becomes the element text', (
      WidgetTester tester,
    ) async {
      await _pump(tester);

      await tester.enterText(
        find.widgetWithText(TextField, 'Email'),
        'jan@example.com',
      );
      await tester.pump();

      expect(_expectFound(Selector(id: 'email_input')).text, 'jan@example.com');
      _expectFound(Selector(text: 'jan@example.com'));
    });

    _testApp('the validation error becomes visible text', (
      WidgetTester tester,
    ) async {
      await _pump(tester);
      expect(_find(Selector(text: 'Email is required')), isNull);

      await tester.tap(find.widgetWithText(FilledButton, 'Log in'));
      await tester.pump();

      _expectFound(Selector(text: 'Email is required'));
    });

    _testApp('relative selectors place the button below the fields', (
      WidgetTester tester,
    ) async {
      await _pump(tester);

      final KNode below = _expectFound(
        Selector(
          id: 'login_button',
          below: Selector(id: 'password_input'),
        ),
      );
      final KNode password = _expectFound(Selector(id: 'password_input'));
      expect(below.bounds.top, greaterThanOrEqualTo(password.bounds.bottom));
      expect(
        _find(
          Selector(
            id: 'login_button',
            above: Selector(id: 'password_input'),
          ),
        ),
        isNull,
      );
    });

    _testApp('the Log in button reports enabled=false while loading', (
      WidgetTester tester,
    ) async {
      await _pump(tester);
      await tester.enterText(find.widgetWithText(TextField, 'Email'), 'a@b.cz');
      await tester.tap(find.widgetWithText(FilledButton, 'Log in'));
      await tester.pump(const Duration(milliseconds: 100));

      // The identifier node wraps the actual button node.
      final KNode button = _expectFound(Selector(id: 'login_button'))
          .children
          .single;
      expect(button.enabled, isFalse);
      expect(button.clickable, isFalse);

      await tester.pump(const Duration(milliseconds: 600));
      await tester.pumpAndSettle();
    });
  });

  group('Products and detail selectors', () {
    Future<void> login(WidgetTester tester) async {
      await tester.enterText(
        find.widgetWithText(TextField, 'Email'),
        'jan@example.com',
      );
      await tester.tap(find.widgetWithText(FilledButton, 'Log in'));
      await tester.pump(const Duration(milliseconds: 700));
      await tester.pumpAndSettle();
    }

    _testApp(
      'ListTile title and subtitle are merged and matched with a regex',
      (WidgetTester tester) async {
        await _pump(tester);
        await login(tester);

        _expectFound(Selector(text: 'Products'));
        // `assertVisible: "Product 1.*"` relies on title + subtitle merging.
        final KNode row = _expectFound(Selector(text: 'Product 1.*'));
        expect(
          row.accessibilityText,
          anyOf('Product 1\n€ 3.00', 'Product 1 € 3.00'),
        );
        // The exact title alone is not the merged label: a literal "Product 1" misses.
        expect(_find(Selector(text: 'Product 1')), isNull);
        // Maestro's newline-to-space retry makes the merged label addressable too.
        _expectFound(Selector(text: 'Product 1 € 3.00'));
      },
    );

    _testApp('the floating action button is found by its tooltip', (
      WidgetTester tester,
    ) async {
      await _pump(tester);
      await login(tester);

      final KNode fab = _expectFound(Selector(text: 'Add product'));
      expect(fab.clickable, isTrue);
    });

    _testApp('the add dialog exposes its title, field and buttons', (
      WidgetTester tester,
    ) async {
      await _pump(tester);
      await login(tester);
      await tester.tap(find.byTooltip('Add product'));
      await tester.pumpAndSettle();

      _expectFound(Selector(text: 'New product'));
      final KNode name = _expectFound(Selector(text: 'Name'));
      expect(name.isTextField, isTrue);
      expect(_expectFound(Selector(text: 'Add')).clickable, isTrue);
      expect(_expectFound(Selector(text: 'Cancel')).clickable, isTrue);

      await tester.tap(find.text('Cancel'));
      await tester.pumpAndSettle();
      expect(
        _find(Selector(text: 'New product')),
        isNull,
        reason: 'assertNotVisible',
      );
    });

    _testApp('the favorite switch reports its checked state', (
      WidgetTester tester,
    ) async {
      await _pump(tester);
      await login(tester);
      await tester.tap(find.text('Product 1'));
      await tester.pumpAndSettle();

      _expectFound(Selector(text: 'Details of Product 1'));
      expect(
        _expectFound(Selector(id: 'favorite_switch')).resourceId,
        'favorite_switch',
      );
      expect(_find(Selector(text: 'Favorite', checked: true)), isNull);
      _expectFound(Selector(text: 'Favorite', checked: false));

      await tester.tap(find.byType(Switch));
      await tester.pump();

      _expectFound(Selector(text: 'Favorite', checked: true));
      expect(_find(Selector(text: 'Favorite', checked: false)), isNull);
    });
  });

  group('F2 playground selectors', () {
    _testApp('every hub entry is reachable by id and by text', (
      WidgetTester tester,
    ) async {
      await _pump(tester);
      await _openPlayground(tester);

      const Map<String, String> entries = <String, String>{
        'nav_selectors_taps': 'Selectors & Taps.*',
        'nav_swipe_scroll': 'Swipe & Scroll.*',
        'nav_text_keys': 'Text & Keys.*',
        'nav_screenshot_theme': 'Screenshot & Theme.*',
        'nav_endless_animation': 'Endless Animation.*',
        'request_location_permission': 'Request Location Permission',
        'nav_network': 'Network.*',
        'nav_app_health': 'App Health.*',
        'nav_app_state': 'App State.*',
      };
      for (final MapEntry<String, String> e in entries.entries) {
        final KNode byId = _expectFound(Selector(id: e.key));
        final KNode byText = _expectFound(Selector(text: e.value));
        expect(byText.bounds.overlaps(byId.bounds), isTrue, reason: e.key);
      }
      expect(
        _expectFound(Selector(id: 'permission_status')).accessibilityText,
        startsWith('Permission: unknown'),
      );
    });

    _testApp(
      'the Traits screen has a 90x90 square and a text longer than 200 characters',
      (WidgetTester tester) async {
        await _pump(tester);
        await _openPlayground(tester);
        await tester.tap(find.text('Selectors & Taps'));
        await tester.pumpAndSettle();

        final KNode square = _expectFound(
          Selector(
            id: 'square_box',
            traits: <ElementTrait>[ElementTrait.square],
          ),
        );
        expect(square.accessibilityText, 'Square box');
        expect(square.bounds.width, closeTo(square.bounds.height, 3));
        expect(square.bounds.width, 270, reason: '90 logical px at 3x');

        final KNode longText = _expectFound(
          Selector(
            id: 'long_text_field',
            traits: <ElementTrait>[ElementTrait.longText],
          ),
        );
        expect(longText.text!.length, greaterThan(200));

        _expectFound(Selector(id: 'split_target'));
        _expectFound(Selector(id: 'retry_button'));
        _expectFound(Selector(text: 'Traits Ready'));

        await tester.pageBack();
        await tester.pumpAndSettle();
      },
    );

    _testApp('size selectors match within their tolerance', (
      WidgetTester tester,
    ) async {
      await _pump(tester);
      await _openPlayground(tester);
      await tester.tap(find.text('Selectors & Taps'));
      await tester.pumpAndSettle();

      _expectFound(Selector(id: 'square_box', width: 270, height: 270));
      expect(
        _find(Selector(id: 'square_box', width: 200, height: 200)),
        isNull,
      );
      _expectFound(
        Selector(id: 'square_box', width: 275, height: 266, tolerance: 6),
      );

      await tester.pageBack();
      await tester.pumpAndSettle();
    });
  });

  group('Snapshot invariants on the sample app', () {
    _testApp(
      'every node has non-negative, finite bounds and parent links are consistent',
      (WidgetTester tester) async {
        await _pump(tester);
        final Snapshot snap = Snapshot.capture();

        expect(snap.nodes, isNotEmpty);
        for (final KNode node in snap.nodes) {
          expect(node.bounds.isFinite, isTrue, reason: '${node.id}');
          expect(
            node.bounds.width,
            greaterThanOrEqualTo(0),
            reason: '${node.id}',
          );
          expect(
            node.bounds.height,
            greaterThanOrEqualTo(0),
            reason: '${node.id}',
          );
          final KNode? parent = node.parent;
          if (parent != null) {
            expect(node.isDescendantOf(parent), isTrue, reason: '${node.id}');
          }
        }
        final Set<int> ids = snap.nodes.map((KNode n) => n.id).toSet();
        expect(ids.length, snap.nodes.length, reason: 'node ids are unique');
      },
    );

    _testApp('node JSON carries the Maestro attribute names', (
      WidgetTester tester,
    ) async {
      await _pump(tester);
      final Map<String, Object?> json = _expectFound(
        Selector(id: 'login_button'),
      ).toJson();

      expect(json['resource-id'], 'login_button');
      // Defaults are omitted on the wire: `enabled` only appears when false.
      expect(json.containsKey('enabled'), isFalse);
      final Map<String, Object?> button = _expectFound(Selector(text: 'Log in'))
          .toJson();
      expect(button['clickable'], true);
      expect(button['accessibilityText'], 'Log in');
      expect(json['bounds'], isA<List<Object?>>());
      expect((json['bounds']! as List<Object?>).length, 4);
    });
  });
}
