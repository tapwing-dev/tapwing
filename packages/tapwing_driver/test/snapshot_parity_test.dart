import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/cupertino.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/platform.dart';
import 'package:tapwing_driver/src/snapshot/snapshot.dart';

const ui.Size _phonePhysicalSize = ui.Size(1206, 2622); // 402x874 @ 3x

void main() {
  void setPhoneViewport(WidgetTester tester) {
    tester.view.physicalSize = _phonePhysicalSize;
    tester.view.devicePixelRatio = 3.0;
    addTearDown(tester.view.reset);
  }

  group('Snapshot parity across Flutter layout and route topologies', () {
    testWidgets('scrolled ListView shifts item bounds and drops scrolled-off rows', (
      WidgetTester tester,
    ) async {
      setPhoneViewport(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      final ScrollController controller = ScrollController();
      addTearDown(controller.dispose);
      try {
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: ListView.builder(
                controller: controller,
                itemCount: 30,
                itemExtent: 100,
                itemBuilder: (BuildContext context, int index) {
                  return Semantics(
                    identifier: 'row_$index',
                    child: Align(
                      alignment: Alignment.centerLeft,
                      child: Text('Row $index'),
                    ),
                  );
                },
              ),
            ),
          ),
        );

        final Snapshot beforeAndroid = Snapshot.capture();
        final KNode row0Before = beforeAndroid.nodes.firstWhere(
          (KNode n) => n.resourceId == 'row_0',
        );
        expect(row0Before.bounds, const Rect.fromLTRB(0, 0, 1206, 300));

        controller.jumpTo(350);
        await tester.pumpAndSettle();

        final Snapshot afterAndroid = Snapshot.capture();
        final Snapshot afterIos = Snapshot.capture(
          platform: DriverPlatform.ios,
          appName: 'Sample App',
        );

        // First three rows (y in -350..-50 pt) are scrolled off-screen; row 4 sits at y = 50..150 pt.
        expect(
          afterAndroid.nodes.any((KNode n) => n.accessibilityText == 'Row 0'),
          isFalse,
        );
        expect(
          afterAndroid.nodes.any((KNode n) => n.accessibilityText == 'Row 1'),
          isFalse,
        );
        expect(
          afterIos.nodes.any((KNode n) => n.accessibilityText == 'Row 0'),
          isFalse,
        );

        final KNode row4Android = afterAndroid.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Row 4',
        );
        final KNode row4Ios = afterIos.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Row 4',
        );
        expect(row4Android.bounds, const Rect.fromLTRB(0, 150, 1206, 450));
        expect(row4Android.text, '');
        expect(row4Ios.bounds, const Rect.fromLTRB(0, 50, 402, 150));
        expect(row4Ios.text, '');
        expect(row4Ios.clickable, isFalse);
      } finally {
        handle.dispose();
      }
    });

    testWidgets('MergeSemantics combines labels while ExcludeSemantics omits hidden subtrees', (
      WidgetTester tester,
    ) async {
      setPhoneViewport(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: Column(
                children: <Widget>[
                  MergeSemantics(
                    child: Semantics(
                      identifier: 'merged_tile',
                      button: true,
                      onTap: () {},
                      child: const Column(
                        children: <Widget>[
                          Text('Primary Title'),
                          Text('Secondary Subtitle'),
                        ],
                      ),
                    ),
                  ),
                  const ExcludeSemantics(
                    child: Text('Hidden Behind Exclude'),
                  ),
                  const Text('Visible Sibling'),
                ],
              ),
            ),
          ),
        );

        final Snapshot android = Snapshot.capture();
        final Snapshot ios = Snapshot.capture(
          platform: DriverPlatform.ios,
          appName: 'Sample App',
        );

        final KNode mergedAndroid = android.nodes.firstWhere(
          (KNode n) => n.resourceId == 'merged_tile',
        );
        expect(
          mergedAndroid.accessibilityText,
          'Primary Title\nSecondary Subtitle',
        );
        expect(mergedAndroid.clickable, isTrue);

        final KNode mergedIos = ios.nodes.firstWhere(
          (KNode n) => n.resourceId == 'merged_tile',
        );
        expect(
          mergedIos.accessibilityText,
          'Primary Title\nSecondary Subtitle',
        );
        expect(mergedIos.clickable, isFalse);

        // Excluded text is absent on both lanes while normal siblings remain visible.
        expect(
          android.nodes.any(
            (KNode n) => n.accessibilityText == 'Hidden Behind Exclude',
          ),
          isFalse,
        );
        expect(
          ios.nodes.any(
            (KNode n) => n.accessibilityText == 'Hidden Behind Exclude',
          ),
          isFalse,
        );
        expect(
          android.nodes.any((KNode n) => n.accessibilityText == 'Visible Sibling'),
          isTrue,
        );
        expect(
          ios.nodes.any((KNode n) => n.accessibilityText == 'Visible Sibling'),
          isTrue,
        );
      } finally {
        handle.dispose();
      }
    });

    testWidgets('modal showDialog scopes semantics to the dialog above underlying routes', (
      WidgetTester tester,
    ) async {
      setPhoneViewport(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: Builder(
                builder: (BuildContext context) {
                  return Center(
                    child: ElevatedButton(
                      onPressed: () {
                        unawaited(
                          showDialog<void>(
                            context: context,
                            builder: (BuildContext ctx) {
                              return AlertDialog(
                                title: const Text('Confirm Delete'),
                                content: const Text('Remove selected item?'),
                                actions: <Widget>[
                                  TextButton(
                                    onPressed: () => Navigator.of(ctx).pop(),
                                    child: const Text('Confirm'),
                                  ),
                                ],
                              );
                            },
                          ),
                        );
                      },
                      child: const Text('Open Underlying Action'),
                    ),
                  );
                },
              ),
            ),
          ),
        );

        expect(
          Snapshot.capture().nodes.any(
            (KNode n) => n.accessibilityText == 'Open Underlying Action',
          ),
          isTrue,
        );

        await tester.tap(find.text('Open Underlying Action'));
        await tester.pumpAndSettle();

        final Snapshot android = Snapshot.capture();
        final Snapshot ios = Snapshot.capture(
          platform: DriverPlatform.ios,
          appName: 'Sample App',
        );

        expect(
          android.nodes.any(
            (KNode n) => n.accessibilityText == 'Open Underlying Action',
          ),
          isFalse,
        );
        expect(
          ios.nodes.any(
            (KNode n) => n.accessibilityText == 'Open Underlying Action',
          ),
          isFalse,
        );

        expect(
          android.nodes.any((KNode n) => n.accessibilityText == 'Confirm Delete'),
          isTrue,
        );
        expect(
          ios.nodes.any((KNode n) => n.accessibilityText == 'Confirm Delete'),
          isTrue,
        );
        expect(
          android.nodes.any((KNode n) => n.accessibilityText == 'Confirm'),
          isTrue,
        );
        expect(
          ios.nodes.any((KNode n) => n.accessibilityText == 'Confirm'),
          isTrue,
        );
      } finally {
        handle.dispose();
      }
    });

    testWidgets('RTL layout mirrors horizontal ordering identically in px and points', (
      WidgetTester tester,
    ) async {
      setPhoneViewport(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        await tester.pumpWidget(
          const MaterialApp(
            home: Directionality(
              textDirection: TextDirection.rtl,
              child: Scaffold(
                body: Row(
                  children: <Widget>[
                    SizedBox(width: 120, height: 48, child: Text('First')),
                    SizedBox(width: 120, height: 48, child: Text('Second')),
                  ],
                ),
              ),
            ),
          ),
        );

        final Snapshot android = Snapshot.capture();
        final Snapshot ios = Snapshot.capture(
          platform: DriverPlatform.ios,
          appName: 'Sample App',
        );

        final KNode aFirst = android.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'First',
        );
        final KNode aSecond = android.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Second',
        );
        final KNode iFirst = ios.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'First',
        );
        final KNode iSecond = ios.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Second',
        );

        expect(aFirst.bounds.left, greaterThan(aSecond.bounds.left));
        expect(iFirst.bounds.left, greaterThan(iSecond.bounds.left));
        expect(aFirst.bounds.right, closeTo(402 * 3.0, 0.5));
        expect(iFirst.bounds.right, closeTo(402, 0.5));
      } finally {
        handle.dispose();
      }
    });

    testWidgets('non-default TextScaler scales element geometry consistently', (
      WidgetTester tester,
    ) async {
      setPhoneViewport(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        Widget buildScaled(double factor) {
          return MaterialApp(
            home: MediaQuery(
              data: MediaQueryData(
                size: const ui.Size(402, 874),
                devicePixelRatio: 3.0,
                textScaler: TextScaler.linear(factor),
              ),
              child: const Scaffold(
                body: Align(
                  alignment: Alignment.topLeft,
                  child: Text('Scalable Headline'),
                ),
              ),
            ),
          );
        }

        await tester.pumpWidget(buildScaled(1.0));
        final Snapshot aBase = Snapshot.capture();
        final Snapshot iBase = Snapshot.capture(
          platform: DriverPlatform.ios,
          appName: 'Sample App',
        );

        await tester.pumpWidget(buildScaled(1.5));
        final Snapshot aScaled = Snapshot.capture();
        final Snapshot iScaled = Snapshot.capture(
          platform: DriverPlatform.ios,
          appName: 'Sample App',
        );

        final KNode aBaseNode = aBase.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Scalable Headline',
        );
        final KNode aScaledNode = aScaled.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Scalable Headline',
        );
        final KNode iBaseNode = iBase.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Scalable Headline',
        );
        final KNode iScaledNode = iScaled.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Scalable Headline',
        );

        expect(
          aScaledNode.bounds.height,
          greaterThan(aBaseNode.bounds.height),
        );
        expect(
          iScaledNode.bounds.height,
          greaterThan(iBaseNode.bounds.height),
        );
        expect(
          aScaledNode.bounds.height / iScaledNode.bounds.height,
          closeTo(3.0, 0.1),
        );
      } finally {
        handle.dispose();
      }
    });

    testWidgets('nested CupertinoPageRoute on iOS scopes to active route and preserves point geometry', (
      WidgetTester tester,
    ) async {
      setPhoneViewport(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        await tester.pumpWidget(
          MaterialApp(
            theme: ThemeData(platform: TargetPlatform.iOS),
            home: Builder(
              builder: (BuildContext context) {
                return Scaffold(
                  body: Center(
                    child: CupertinoButton(
                      onPressed: () {
                        unawaited(
                          Navigator.of(context).push<void>(
                            CupertinoPageRoute<void>(
                              builder: (_) => Navigator(
                                onGenerateRoute: (_) => CupertinoPageRoute<void>(
                                  builder: (BuildContext nestedCtx) {
                                    return CupertinoPageScaffold(
                                      navigationBar: const CupertinoNavigationBar(
                                        middle: Text('Detail Page'),
                                      ),
                                      child: SafeArea(
                                        child: Align(
                                          alignment: Alignment.topLeft,
                                          child: CupertinoButton(
                                            onPressed: () {},
                                            child: const Text('Save Item'),
                                          ),
                                        ),
                                      ),
                                    );
                                  },
                                ),
                              ),
                            ),
                          ),
                        );
                      },
                      child: const Text('Home Page'),
                    ),
                  ),
                );
              },
            ),
          ),
        );

        await tester.tap(find.text('Home Page'));
        await tester.pumpAndSettle();

        final Snapshot iosSnap = Snapshot.capture(
          platform: DriverPlatform.ios,
          appName: 'Sample App',
        );
        final KNode root = iosSnap.root!;
        expect(root.accessibilityText, 'Sample App');
        expect(root.bounds, const Rect.fromLTRB(0, 0, 402, 874));

        // Underlying Home Page is no longer visible in active route semantics.
        expect(
          iosSnap.nodes.any((KNode n) => n.accessibilityText == 'Home Page'),
          isFalse,
        );

        final KNode detail = iosSnap.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Detail Page',
        );
        final KNode save = iosSnap.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Save Item',
        );
        expect(detail.isDescendantOf(root), isTrue);
        expect(save.isDescendantOf(root), isTrue);
        expect(save.clickable, isFalse);
        expect(save.bounds.right, lessThanOrEqualTo(402));
        expect(save.bounds.bottom, lessThanOrEqualTo(874));
      } finally {
        handle.dispose();
      }
    });
  });
}
