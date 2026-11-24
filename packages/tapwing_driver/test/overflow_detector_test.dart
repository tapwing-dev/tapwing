// Differential tests for OverflowDetector: `flutter test` runs in debug mode,
// where Flutter reports every painted overflow through FlutterError.onError.
// The detector (built for profile/release builds, where that report is
// compiled out) must find exactly the same render objects with exactly the
// same messages.
import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/diagnostics/overflow_detector.dart';

/// The overflowing render object named in a debug overflow report
/// (`describeForError('The specific RenderFlex in question is')`).
RenderObject ownerOf(FlutterErrorDetails details) => details
    .informationCollector!()
    .map((DiagnosticsNode node) => node.value)
    .whereType<RenderObject>()
    .first;

/// Pumps [child] and returns Flutter's debug overflow reports as
/// `render object -> message`.
Future<Map<RenderObject, String>> pumpCollectingReports(
  WidgetTester tester,
  Widget child, {
  bool wrap = true,
}) async {
  final Map<RenderObject, String> reports = <RenderObject, String>{};
  final FlutterExceptionHandler? previous = FlutterError.onError;
  FlutterError.onError = (FlutterErrorDetails details) {
    final String message = details.exceptionAsString();
    if (!message.contains('overflowed by')) {
      previous?.call(details);
      return;
    }
    reports[ownerOf(details)] = message;
  };
  try {
    await tester.pumpWidget(
      wrap
          ? MaterialApp(
              debugShowCheckedModeBanner: false,
              home: Scaffold(
                body: Align(alignment: Alignment.topLeft, child: child),
              ),
            )
          : child,
    );
  } finally {
    FlutterError.onError = previous;
  }
  return reports;
}

Map<RenderObject, String> detect() => <RenderObject, String>{
  for (final LayoutOverflow o in OverflowDetector.scan())
    o.renderObject: o.message,
};

/// Asserts the detector matches Flutter's debug reports exactly and that the
/// scenario produced [expected] reports (guards against scenarios that stop
/// overflowing after framework changes).
Future<void> expectParity(
  WidgetTester tester,
  Widget child, {
  required int expected,
}) async {
  final Map<RenderObject, String> debug = await pumpCollectingReports(
    tester,
    child,
  );
  expect(
    debug,
    hasLength(expected),
    reason: 'scenario should produce $expected debug report(s)',
  );
  expect(detect(), equals(debug));
}

const Widget longText = Text(
  'A very long product name that does not fit into the row at all',
);

void main() {
  group('reported overflows (parity with debug mode)', () {
    testWidgets('Row with long text overflows on the right', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 200,
          child: Row(children: <Widget>[Icon(Icons.star), longText]),
        ),
        expected: 1,
      );
    });

    testWidgets('Column overflows on the bottom', (WidgetTester tester) async {
      await expectParity(
        tester,
        const SizedBox(
          height: 100,
          child: Column(
            children: <Widget>[
              SizedBox(width: 50, height: 50),
              SizedBox(width: 50, height: 50),
              SizedBox(width: 50, height: 50),
            ],
          ),
        ),
        expected: 1,
      );
    });

    testWidgets('spacing counts towards the overflow', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 200,
          child: Row(
            spacing: 20,
            children: <Widget>[
              SizedBox(width: 60, height: 10),
              SizedBox(width: 60, height: 10),
              SizedBox(width: 60, height: 10),
            ],
          ),
        ),
        expected: 1,
      );
    });

    testWidgets('fixed child next to Expanded', (WidgetTester tester) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 100,
          child: Row(
            children: <Widget>[
              Expanded(child: SizedBox()),
              SizedBox(width: 150, height: 10),
            ],
          ),
        ),
        expected: 1,
      );
    });

    testWidgets('sub-pixel overflow uses precision formatting', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 100,
          child: Row(children: <Widget>[SizedBox(width: 100.5, height: 10)]),
        ),
        expected: 1,
      );
      expect(
        detect().values.single,
        'A RenderFlex overflowed by 0.500 pixels on the right.',
      );
    });

    testWidgets('RTL row still reports the right edge like Flutter', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        const Directionality(
          textDirection: TextDirection.rtl,
          child: SizedBox(width: 200, child: Row(children: <Widget>[longText])),
        ),
        expected: 1,
      );
    });

    testWidgets('UnconstrainedBox overflows on both sides', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 100,
          height: 50,
          child: UnconstrainedBox(child: SizedBox(width: 200, height: 20)),
        ),
        expected: 1,
      );
      expect(
        detect().values.single,
        'A RenderConstraintsTransformBox overflowed by 50 pixels on the left and 50 pixels on the right.',
      );
    });

    testWidgets('nested overflows are all reported', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 150,
          height: 60,
          child: Column(
            children: <Widget>[
              Row(children: <Widget>[longText]),
              SizedBox(height: 40),
              Row(children: <Widget>[SizedBox(width: 400, height: 10)]),
            ],
          ),
        ),
        expected: 3,
      );
    });

    testWidgets('ExcludeSemantics still paints, so it is reported', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        const ExcludeSemantics(
          child: SizedBox(width: 120, child: Row(children: <Widget>[longText])),
        ),
        expected: 1,
      );
    });

    testWidgets('partially visible list item is reported', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        SizedBox(
          height: 150,
          width: 200,
          child: ListView(
            itemExtent: 100,
            children: <Widget>[
              for (int i = 0; i < 3; i++)
                const Row(children: <Widget>[longText]),
            ],
          ),
        ),
        // Item 0 and the half-visible item 1; item 2 is only in the cache extent.
        expected: 2,
      );
    });

    testWidgets('overflow inside a dialog route', (WidgetTester tester) async {
      await pumpCollectingReports(
        tester,
        Builder(
          builder: (BuildContext context) => TextButton(
            onPressed: () => showDialog<void>(
              context: context,
              builder: (_) => const Dialog(
                child: SizedBox(
                  width: 120,
                  child: Row(children: <Widget>[longText]),
                ),
              ),
            ),
            child: const Text('open'),
          ),
        ),
      );
      final Map<RenderObject, String> reports = <RenderObject, String>{};
      final FlutterExceptionHandler? previous = FlutterError.onError;
      FlutterError.onError = (FlutterErrorDetails d) =>
          reports[ownerOf(d)] = d.exceptionAsString();
      try {
        await tester.tap(find.text('open'));
        await tester.pumpAndSettle();
      } finally {
        FlutterError.onError = previous;
      }
      expect(reports, hasLength(1));
      expect(detect(), equals(reports));
    });
  });

  group('not reported (not painted, matching debug mode)', () {
    testWidgets('Offstage', (WidgetTester tester) async {
      await expectParity(
        tester,
        const Offstage(
          child: SizedBox(width: 120, child: Row(children: <Widget>[longText])),
        ),
        expected: 0,
      );
    });

    testWidgets('zero opacity', (WidgetTester tester) async {
      await expectParity(
        tester,
        const Opacity(
          opacity: 0,
          alwaysIncludeSemantics: true,
          child: SizedBox(width: 120, child: Row(children: <Widget>[longText])),
        ),
        expected: 0,
      );
    });

    testWidgets('hidden IndexedStack child', (WidgetTester tester) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 120,
          height: 40,
          child: IndexedStack(
            children: <Widget>[
              SizedBox(),
              Row(children: <Widget>[longText]),
            ],
          ),
        ),
        expected: 0,
      );
    });

    testWidgets('cache-extent list item below the viewport', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        SizedBox(
          height: 100,
          width: 200,
          child: ListView(
            itemExtent: 100,
            children: const <Widget>[
              SizedBox(),
              Row(children: <Widget>[longText]),
            ],
          ),
        ),
        expected: 0,
      );
    });

    testWidgets('UnconstrainedBox that clips', (WidgetTester tester) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 100,
          height: 50,
          child: UnconstrainedBox(
            clipBehavior: Clip.hardEdge,
            child: SizedBox(width: 200, height: 20),
          ),
        ),
        expected: 0,
      );
    });

    testWidgets('Expanded text with ellipsis fits', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 120,
          child: Row(
            children: <Widget>[
              Expanded(
                child: Text(
                  'Long text that is ellipsized',
                  overflow: TextOverflow.ellipsis,
                ),
              ),
            ],
          ),
        ),
        expected: 0,
      );
    });

    testWidgets('Row in a horizontal scroll view has unbounded width', (
      WidgetTester tester,
    ) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 120,
          child: SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            child: Row(children: <Widget>[longText]),
          ),
        ),
        expected: 0,
      );
    });

    testWidgets('zero-size flex is never painted', (WidgetTester tester) async {
      await expectParity(
        tester,
        const SizedBox(
          width: 0,
          height: 20,
          child: Row(children: <Widget>[SizedBox(width: 50)]),
        ),
        expected: 0,
      );
    });

    testWidgets('route covered by an opaque route', (
      WidgetTester tester,
    ) async {
      final GlobalKey<NavigatorState> nav = GlobalKey<NavigatorState>();
      await pumpCollectingReports(
        tester,
        MaterialApp(
          navigatorKey: nav,
          home: const Scaffold(
            body: Align(
              alignment: Alignment.topLeft,
              child: SizedBox(
                width: 120,
                child: Row(children: <Widget>[longText]),
              ),
            ),
          ),
        ),
        wrap: false,
      );
      expect(detect(), hasLength(1));
      unawaited(
        nav.currentState!.push(
          MaterialPageRoute<void>(builder: (_) => const Scaffold()),
        ),
      );
      await tester.pumpAndSettle();
      // The first route is now offstage: not painted, so nothing visible.
      expect(detect(), isEmpty);
    });
  });

  testWidgets('intended difference: painted but scrolled out of view', (
    WidgetTester tester,
  ) async {
    // SingleChildScrollView paints its whole child, so debug mode reports an
    // overflow far below the fold. The detector reports only what is visible
    // through ancestor clips, so it waits until the user scrolls to it.
    final Map<RenderObject, String> debug = await pumpCollectingReports(
      tester,
      const SizedBox(
        width: 200,
        height: 100,
        child: SingleChildScrollView(
          child: Column(
            children: <Widget>[
              SizedBox(height: 300),
              Row(children: <Widget>[longText]),
            ],
          ),
        ),
      ),
    );
    expect(debug, hasLength(1));
    expect(detect(), isEmpty);
    await tester.drag(
      find.byType(SingleChildScrollView),
      const Offset(0, -300),
    );
    await tester.pump();
    expect(detect(), equals(debug));
  });

  testWidgets('reports bounds and first text for locating the overflow', (
    WidgetTester tester,
  ) async {
    await pumpCollectingReports(
      tester,
      const Padding(
        padding: EdgeInsets.only(left: 10, top: 20),
        child: SizedBox(
          width: 120,
          height: 30,
          child: Row(children: <Widget>[Icon(Icons.star), longText]),
        ),
      ),
    );
    final LayoutOverflow overflow = OverflowDetector.scan().single;
    expect(overflow.globalBounds, const Rect.fromLTWH(10, 20, 120, 30));
    expect(overflow.text, startsWith('A very long product name'));
    expect(overflow.overflow.right, greaterThan(0));
    expect(
      OverflowDetector.formatRect(overflow.globalBounds),
      '10.0,20.0 120.0x30.0',
    );
  });

  test('message formatting matches DebugOverflowIndicatorMixin', () {
    expect(
      OverflowDetector.overflowMessage(
        'RenderFlex',
        const RelativeRect.fromLTRB(0, 0, 12.4, 0),
      ),
      'A RenderFlex overflowed by 12 pixels on the right.',
    );
    expect(
      OverflowDetector.overflowMessage(
        'RenderFlex',
        const RelativeRect.fromLTRB(0, 0, 0, 3.26),
      ),
      'A RenderFlex overflowed by 3.3 pixels on the bottom.',
    );
    expect(
      OverflowDetector.overflowMessage(
        'X',
        const RelativeRect.fromLTRB(1.5, 2, 3, 4),
      ),
      'A X overflowed by 1.5 pixels on the left, 2.0 pixels on the top, '
      '4.0 pixels on the bottom, and 3.0 pixels on the right.',
    );
  });
}
