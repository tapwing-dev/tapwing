import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/snapshot/snapshot.dart';

void main() {
  group('Snapshot semantics composition (widget tests)', () {
    testWidgets('text field composes text=value and hintText="label, hint"', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      final TextEditingController emailController = TextEditingController(
        text: 'jan@example.com',
      );
      final TextEditingController emptyController = TextEditingController();
      try {
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: Column(
                children: <Widget>[
                  TextField(
                    controller: emailController,
                    decoration: const InputDecoration(labelText: 'Email'),
                  ),
                  TextField(
                    controller: emptyController,
                    decoration: const InputDecoration(labelText: 'Name'),
                  ),
                  Semantics(
                    identifier: 'composed_field',
                    textField: true,
                    value: 'jan@example.com',
                    label: 'Email',
                    hint: 'you@domain.com',
                    child: const SizedBox(width: 200, height: 48),
                  ),
                ],
              ),
            ),
          ),
        );

        final Snapshot snap = Snapshot.capture();
        final List<KNode> fields = snap.nodes
            .where((KNode n) => n.isTextField)
            .toList();
        expect(fields.length, 3);

        expect(fields[0].text, 'jan@example.com');
        expect(fields[0].hintText, 'Email');
        expect(fields[0].accessibilityText, isNull);

        expect(fields[1].text, '');
        expect(fields[1].hintText, 'Name');

        final KNode composed = snap.nodes.firstWhere(
          (KNode n) => n.resourceId == 'composed_field',
        );
        expect(composed.text, 'jan@example.com');
        expect(composed.hintText, 'Email, you@domain.com');
        expect(composed.accessibilityText, isNull);
      } finally {
        emailController.dispose();
        emptyController.dispose();
        handle.dispose();
      }
    });

    testWidgets(
      'non-text-field nodes compose accessibilityText="value, label, hint" and flags',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: Column(
                  children: <Widget>[
                    Semantics(
                      identifier: 'volume_control',
                      value: '50%',
                      label: 'Volume',
                      hint: 'Double tap to mute',
                      child: const SizedBox(width: 120, height: 48),
                    ),
                    Semantics(
                      identifier: 'disabled_action',
                      enabled: false,
                      label: 'Disabled button',
                      child: const SizedBox(width: 120, height: 48),
                    ),
                    Semantics(
                      identifier: 'hidden_node',
                      hidden: true,
                      label: 'Should be skipped',
                      child: const SizedBox(width: 120, height: 48),
                    ),
                    SwitchListTile(
                      title: const Text('Favorite'),
                      value: true,
                      onChanged: (_) {},
                    ),
                  ],
                ),
              ),
            ),
          );

          final Snapshot snap = Snapshot.capture();
          final KNode volume = snap.nodes.firstWhere(
            (KNode n) => n.resourceId == 'volume_control',
          );
          // Android ViewHierarchy.kt:127 / AndroidDriver.kt:1152 always emits text="".
          expect(volume.text, '');
          expect(volume.hintText, isNull);
          expect(volume.accessibilityText, '50%, Volume, Double tap to mute');

          final KNode disabled = snap.nodes.firstWhere(
            (KNode n) => n.resourceId == 'disabled_action',
          );
          expect(disabled.enabled, isFalse);

          expect(
            snap.nodes.where((KNode n) => n.resourceId == 'hidden_node'),
            isEmpty,
          );

          final KNode favorite = snap.nodes.firstWhere(
            (KNode n) => n.accessibilityText == 'Favorite',
          );
          expect(favorite.checked, isTrue);
          expect(favorite.clickable, isTrue);
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets(
      'Snapshot.computeHash is stable when unchanged and changes on state update',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          bool checked = false;
          late StateSetter rebuild;

          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: StatefulBuilder(
                  builder: (BuildContext context, StateSetter setState) {
                    rebuild = setState;
                    return CheckboxListTile(
                      title: const Text('Accept terms'),
                      value: checked,
                      onChanged: (_) {},
                    );
                  },
                ),
              ),
            ),
          );

          final int hashBefore = Snapshot.computeHash();
          await tester.pump();
          expect(Snapshot.computeHash(), hashBefore);

          rebuild(() => checked = true);
          await tester.pump();
          expect(Snapshot.computeHash(), isNot(hashBefore));
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets(
      'OverlayPortal grafted children keep true screen bounds (no double transform)',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final OverlayPortalController portalController =
            OverlayPortalController();
        final LayerLink link = LayerLink();
        try {
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: Padding(
                  padding: const EdgeInsets.only(left: 50, top: 300),
                  child: CompositedTransformTarget(
                    link: link,
                    child: OverlayPortal(
                      controller: portalController,
                      overlayChildBuilder: (BuildContext context) =>
                          CompositedTransformFollower(
                            link: link,
                            offset: const Offset(0, 60),
                            child: Align(
                              alignment: Alignment.topLeft,
                              child: Semantics(
                                identifier: 'portal_item',
                                button: true,
                                label: 'Portal Option',
                                child: const SizedBox(width: 200, height: 48),
                              ),
                            ),
                          ),
                      child: Semantics(
                        identifier: 'portal_trigger',
                        button: true,
                        label: 'Open Portal',
                        child: const SizedBox(width: 200, height: 48),
                      ),
                    ),
                  ),
                ),
              ),
            ),
          );

          portalController.show();
          await tester.pumpAndSettle();

          final Snapshot snap = Snapshot.capture();
          expect(Snapshot.computeHash(), snap.hash);

          final KNode trigger = snap.nodes.firstWhere(
            (KNode n) => n.resourceId == 'portal_trigger',
          );
          final KNode item = snap.nodes.firstWhere(
            (KNode n) => n.resourceId == 'portal_item',
          );
          final double dpr = tester.view.devicePixelRatio;
          // Trigger is at logical (50, 300); follower item is at (50, 360),
          // NOT double-translated to (100, 660).
          expect(trigger.bounds.left, closeTo(50 * dpr, 1.0));
          expect(trigger.bounds.top, closeTo(300 * dpr, 1.0));
          expect(item.bounds.left, closeTo(50 * dpr, 1.0));
          expect(item.bounds.top, closeTo(360 * dpr, 1.0));
          expect(snap.isVisible(item), isTrue);
        } finally {
          handle.dispose();
        }
      },
    );
  });
}
