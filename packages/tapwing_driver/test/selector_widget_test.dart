import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/selector/selector.dart';
import 'package:tapwing_driver/src/snapshot/snapshot.dart';

void main() {
  group('Selector resolution on a real widget tree', () {
    testWidgets(
      'resolves text, id, ListTile newline-to-space, index, and relative selectors',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                appBar: AppBar(title: const Text('Products')),
                body: Column(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: <Widget>[
                    const ListTile(
                      title: Text('Product 1'),
                      subtitle: Text('€ 3.00'),
                    ),
                    Semantics(
                      identifier: 'action_row',
                      container: true,
                      child: Row(
                        children: <Widget>[
                          ElevatedButton(
                            onPressed: () {},
                            child: const Text('Action'),
                          ),
                          const SizedBox(width: 16),
                          ElevatedButton(
                            onPressed: () {},
                            child: const Text('Action'),
                          ),
                        ],
                      ),
                    ),
                    Semantics(
                      identifier: 'login_button',
                      container: true,
                      child: ElevatedButton(
                        onPressed: () {},
                        child: const Text('Log in'),
                      ),
                    ),
                  ],
                ),
              ),
            ),
          );

          final Snapshot snap = Snapshot.capture();

          // ListTile merges title + subtitle with '\n'.
          expect(resolve(Selector(text: 'Product 1'), snap), isNull);
          expect(resolve(Selector(text: 'Product 1.*'), snap), isNotNull);
          expect(resolve(Selector(text: 'Product 1 € 3.00'), snap), isNotNull);

          // Identifier resolution.
          final KNode? loginBtn = resolve(Selector(id: 'login_button'), snap);
          expect(loginBtn, isNotNull);

          // Index orders left-to-right on the same row.
          final KNode? firstAction = resolve(
            Selector(text: 'Action', index: 0),
            snap,
          );
          final KNode? secondAction = resolve(
            Selector(text: 'Action', index: 1),
            snap,
          );
          expect(firstAction, isNotNull);
          expect(secondAction, isNotNull);
          expect(firstAction!.bounds.left, lessThan(secondAction!.bounds.left));

          // Relative selector: Log in is below Products header.
          expect(
            resolve(
              Selector(
                text: 'Log in',
                below: Selector(text: 'Products'),
              ),
              snap,
            ),
            isNotNull,
          );
          expect(
            resolve(
              Selector(
                text: 'Log in',
                above: Selector(text: 'Products'),
              ),
              snap,
            ),
            isNull,
          );

          // childOf scopes inside action_row.
          expect(
            resolve(
              Selector(
                text: 'Action',
                childOf: Selector(id: 'action_row'),
              ),
              snap,
            ),
            isNotNull,
          );
          expect(
            resolve(
              Selector(
                text: 'Log in',
                childOf: Selector(id: 'action_row'),
              ),
              snap,
            ),
            isNull,
          );
        } finally {
          handle.dispose();
        }
      },
    );
  });
}
