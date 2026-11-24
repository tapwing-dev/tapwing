import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';

void main() {
  DriverContext makeContext(
    WidgetTester tester, {
    TextInputController? textInput,
    ui.Brightness Function()? readPlatformBrightness,
    double Function()? readBottomInset,
  }) {
    final DateTime epoch = tester.binding.clock.now();
    final TextInputController ti = textInput ?? TextInputController();
    final IdleDetector idle = IdleDetector(
      httpTracker: HttpTracker(),
      textInput: ti,
      readBottomInset: readBottomInset ?? () => 0,
      waitForFirstFrame: (_) async {},
    );
    return DriverContext(
      idle: idle,
      pointer: PointerInput(
        clock: () => tester.binding.clock.now().difference(epoch),
      ),
      textInput: ti,
      readPlatformBrightness: readPlatformBrightness,
      readBottomInset: readBottomInset ?? () => 0,
    );
  }

  /// Drives FakeAsync frames until [future] completes.
  Future<T> runDriver<T>(WidgetTester tester, Future<T> future) async {
    T? result;
    Object? err;
    StackTrace? st;
    bool done = false;
    unawaited(
      future.then(
        (T value) {
          result = value;
          done = true;
        },
        onError: (Object e, StackTrace s) {
          err = e;
          st = s;
          done = true;
        },
      ),
    );
    for (int i = 0; i < 400 && !done; i++) {
      await tester.pump(const Duration(milliseconds: 16));
    }
    expect(done, isTrue, reason: 'Driver RPC did not complete');
    if (err != null) Error.throwWithStackTrace(err!, st!);
    return result as T;
  }

  group('F2 driver capabilities (widget tests)', () {
    testWidgets('tapOn relativePoint (% and px) hits exact region of element', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        String lastHit = 'none';
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: Center(
                child: Semantics(
                  identifier: 'split_bar',
                  container: true,
                  child: SizedBox(
                    width: 200,
                    height: 80,
                    child: Row(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: <Widget>[
                        Expanded(
                          child: GestureDetector(
                            behavior: HitTestBehavior.opaque,
                            onTap: () => lastHit = 'left',
                            child: const ColoredBox(color: Colors.red),
                          ),
                        ),
                        Expanded(
                          child: GestureDetector(
                            behavior: HitTestBehavior.opaque,
                            onTap: () => lastHit = 'right',
                            child: const ColoredBox(color: Colors.blue),
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            ),
          ),
        );

        final CommandDispatcher dispatcher = CommandDispatcher(
          makeContext(tester),
        );

        await runDriver<Object?>(
          tester,
          dispatcher.dispatch('tapOn', <String, Object?>{
            'selector': <String, Object?>{'id': 'split_bar'},
            'relativePoint': '85%, 50%',
            'timeoutMs': 1000,
            'settleTimeoutMs': 200,
          }),
        );
        expect(lastHit, 'right');

        await runDriver<Object?>(
          tester,
          dispatcher.dispatch('tapOn', <String, Object?>{
            'selector': <String, Object?>{'id': 'split_bar'},
            'relativePoint': '10, 40',
            'timeoutMs': 1000,
            'settleTimeoutMs': 200,
          }),
        );
        expect(lastHit, 'left');
      } finally {
        handle.dispose();
      }
    });

    testWidgets(
      'tapOn retryIfNoChange retries once when UI does not change, and stops when it does',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          int silentTaps = 0;
          int changingTaps = 0;
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: StatefulBuilder(
                  builder: (BuildContext context, StateSetter setState) =>
                      Column(
                        children: <Widget>[
                          ElevatedButton(
                            onPressed: () => silentTaps++,
                            child: const Text('Silent'),
                          ),
                          ElevatedButton(
                            onPressed: () => setState(() => changingTaps++),
                            child: Text('Changing $changingTaps'),
                          ),
                        ],
                      ),
                ),
              ),
            ),
          );

          final CommandDispatcher dispatcher = CommandDispatcher(
            makeContext(tester),
          );

          // Does not mutate semantics tree -> retried once (total 2 taps).
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('tapOn', <String, Object?>{
              'selector': <String, Object?>{'text': 'Silent'},
              'retryIfNoChange': true,
              'timeoutMs': 500,
              'settleTimeoutMs': 200,
            }),
          );
          expect(silentTaps, 2);

          // Mutates semantics tree on first tap -> only 1 tap.
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('tapOn', <String, Object?>{
              'selector': <String, Object?>{'text': 'Changing 0'},
              'retryIfNoChange': true,
              'timeoutMs': 500,
              'settleTimeoutMs': 200,
            }),
          );
          expect(changingTaps, 1);
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets(
      'tapOn with repeat=2 triggers Flutter GestureDetector.onDoubleTap',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          int doubleTaps = 0;
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: Center(
                  child: GestureDetector(
                    onDoubleTap: () => doubleTaps++,
                    child: const Padding(
                      padding: EdgeInsets.all(24),
                      child: Text('Double target'),
                    ),
                  ),
                ),
              ),
            ),
          );

          final CommandDispatcher dispatcher = CommandDispatcher(
            makeContext(tester),
          );
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('tapOn', <String, Object?>{
              'selector': <String, Object?>{'text': 'Double target'},
              'repeat': 2,
              'delayMs': 100,
              'timeoutMs': 500,
              'settleTimeoutMs': 200,
            }),
          );
          expect(doubleTaps, 1);
          // Drain DoubleTapGestureRecognizer's 40 ms countdown timer.
          await tester.pump(const Duration(milliseconds: 50));
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets(
      'tapOn waitUntilVisible waits for covering overlay to clear and re-taps',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          bool showOverlay = true;
          int targetTaps = 0;
          late StateSetter rebuild;

          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: StatefulBuilder(
                  builder: (BuildContext context, StateSetter setState) {
                    rebuild = setState;
                    return Stack(
                      children: <Widget>[
                        Positioned(
                          left: 40,
                          top: 40,
                          width: 200,
                          height: 80,
                          child: ElevatedButton(
                            onPressed: () => setState(() => targetTaps++),
                            child: Text('Underneath $targetTaps'),
                          ),
                        ),
                        if (showOverlay)
                          Positioned(
                            left: 40,
                            top: 40,
                            width: 200,
                            height: 80,
                            child: Semantics(
                              container: true,
                              label: 'Overlay',
                              child: GestureDetector(
                                behavior: HitTestBehavior.opaque,
                                onTap: () {},
                                child: const ColoredBox(color: Colors.amber),
                              ),
                            ),
                          ),
                      ],
                    );
                  },
                ),
              ),
            ),
          );

          // Remove overlay after 120 ms so waitUntilVisible catches it.
          Timer(const Duration(milliseconds: 120), () {
            rebuild(() => showOverlay = false);
          });

          final CommandDispatcher dispatcher = CommandDispatcher(
            makeContext(tester),
          );
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('tapOn', <String, Object?>{
              'selector': <String, Object?>{'text': 'Underneath 0'},
              'waitUntilVisible': true,
              'timeoutMs': 500,
              'settleTimeoutMs': 100,
            }),
          );
          expect(targetTaps, 1);
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets('swipe from element dismisses a Dismissible item', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        bool dismissed = false;
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: StatefulBuilder(
                builder: (BuildContext context, StateSetter setState) =>
                    dismissed
                    ? const Text('Item removed')
                    : Dismissible(
                        key: const ValueKey<String>('item_1'),
                        direction: DismissDirection.endToStart,
                        onDismissed: (_) => setState(() => dismissed = true),
                        child: const ListTile(title: Text('Swipe me left')),
                      ),
              ),
            ),
          ),
        );

        final CommandDispatcher dispatcher = CommandDispatcher(
          makeContext(tester),
        );
        await runDriver<Object?>(
          tester,
          dispatcher.dispatch('swipe', <String, Object?>{
            'selector': <String, Object?>{'text': 'Swipe me left'},
            'direction': 'LEFT',
            'relativePoint': '80%, 50%',
            'durationMs': 300,
            'timeoutMs': 500,
            'settleTimeoutMs': 500,
          }),
        );
        // Let Dismissible's slide + resize animations finish under FakeAsync.
        await tester.pumpAndSettle();
        expect(dismissed, isTrue);
      } finally {
        handle.dispose();
      }
    });

    testWidgets(
      'scrollUntilVisible supports horizontal RIGHT scroll and centerElement',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: Center(
                  child: SizedBox(
                    height: 160,
                    child: ListView.builder(
                      scrollDirection: Axis.horizontal,
                      itemCount: 30,
                      itemBuilder: (BuildContext context, int i) => SizedBox(
                        width: 140,
                        child: Card(
                          child: Center(child: Text('Card ${i + 1}')),
                        ),
                      ),
                    ),
                  ),
                ),
              ),
            ),
          );

          final CommandDispatcher dispatcher = CommandDispatcher(
            makeContext(tester),
          );
          final Map<String, Object?> result =
              (await runDriver<Object?>(
                    tester,
                    dispatcher.dispatch('scrollUntilVisible', <String, Object?>{
                      'selector': <String, Object?>{'text': 'Card 15'},
                      'direction': 'RIGHT',
                      'durationMs': 250,
                      'centerElement': true,
                      'visibilityPercentage': 100,
                      'timeoutMs': 10000,
                      'settleTimeoutMs': 300,
                    }),
                  ))!
                  as Map<String, Object?>;
          expect(result['swipes']! as int, greaterThan(0));
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets(
      'copyTextFrom follows Orchestra.resolveText priority (text -> hintText -> accessibilityText)',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final TextEditingController filled = TextEditingController(
          text: 'filled@example.com',
        );
        final TextEditingController empty = TextEditingController();
        try {
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: Column(
                  children: <Widget>[
                    Semantics(
                      identifier: 'filled_field',
                      child: TextField(
                        controller: filled,
                        decoration: const InputDecoration(labelText: 'Email'),
                      ),
                    ),
                    Semantics(
                      identifier: 'empty_field',
                      child: TextField(
                        controller: empty,
                        decoration: const InputDecoration(
                          labelText: 'Placeholder Label',
                        ),
                      ),
                    ),
                    const Text('Plain Label'),
                  ],
                ),
              ),
            ),
          );

          final CommandDispatcher dispatcher = CommandDispatcher(
            makeContext(tester),
          );

          final Map<String, Object?> r1 =
              (await runDriver<Object?>(
                    tester,
                    dispatcher.dispatch('copyTextFrom', <String, Object?>{
                      'selector': <String, Object?>{
                        'text': 'filled@example.com',
                      },
                      'timeoutMs': 500,
                    }),
                  ))!
                  as Map<String, Object?>;
          expect(r1['text'], 'filled@example.com');

          // Empty TextField has text="" and hintText="Placeholder Label" -> must return hintText!
          final Map<String, Object?> r2 =
              (await runDriver<Object?>(
                    tester,
                    dispatcher.dispatch('copyTextFrom', <String, Object?>{
                      'selector': <String, Object?>{
                        'text': 'Placeholder Label',
                      },
                      'timeoutMs': 500,
                    }),
                  ))!
                  as Map<String, Object?>;
          expect(r2['text'], 'Placeholder Label');

          final Map<String, Object?> r3 =
              (await runDriver<Object?>(
                    tester,
                    dispatcher.dispatch('copyTextFrom', <String, Object?>{
                      'selector': <String, Object?>{'text': 'Plain Label'},
                      'timeoutMs': 500,
                    }),
                  ))!
                  as Map<String, Object?>;
          expect(r3['text'], 'Plain Label');
        } finally {
          filled.dispose();
          empty.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets(
      'pressKey handles ENTER, BACKSPACE, TAB, ESCAPE, and BACK at engine boundary',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        final TextEditingController firstCtrl = TextEditingController();
        final TextEditingController secondCtrl = TextEditingController();
        final FocusNode firstFocus = FocusNode();
        final FocusNode secondFocus = FocusNode();
        String? submitted;
        try {
          final TextInputController textInput = TextInputController();
          // Observe TextInput.setClient messages from the test messenger.
          tester.binding.defaultBinaryMessenger.setMockMessageHandler(
            SystemChannels.textInput.name,
            (ByteData? message) async {
              if (message != null) textInput.onOutgoing(message);
              return null;
            },
          );

          await tester.pumpWidget(
            MaterialApp(
              home: Builder(
                builder: (BuildContext context) => Scaffold(
                  body: Column(
                    children: <Widget>[
                      TextField(
                        controller: firstCtrl,
                        focusNode: firstFocus,
                        textInputAction: TextInputAction.done,
                        onSubmitted: (String v) => submitted = v,
                        decoration: const InputDecoration(labelText: 'First'),
                      ),
                      TextField(
                        controller: secondCtrl,
                        focusNode: secondFocus,
                        decoration: const InputDecoration(labelText: 'Second'),
                      ),
                      ElevatedButton(
                        onPressed: () {
                          unawaited(
                            showDialog<void>(
                              context: context,
                              builder: (BuildContext ctx) => AlertDialog(
                                title: const Text('Popup Dialog'),
                                actions: <Widget>[
                                  TextButton(
                                    autofocus: true,
                                    onPressed: () => Navigator.pop(ctx),
                                    child: const Text('Close'),
                                  ),
                                ],
                              ),
                            ),
                          );
                        },
                        child: const Text('Open Dialog'),
                      ),
                    ],
                  ),
                ),
              ),
            ),
          );

          final CommandDispatcher dispatcher = CommandDispatcher(
            makeContext(tester, textInput: textInput),
          );

          // Focus first field, type "hello", press BACKSPACE -> "hell", press ENTER -> onSubmitted("hell").
          // settleTimeoutMs: 0 skips real-time KeyboardWaiter under FakeAsync.
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('tapOn', <String, Object?>{
              'selector': <String, Object?>{'text': 'First'},
              'timeoutMs': 500,
              'settleTimeoutMs': 0,
            }),
          );
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('inputText', <String, Object?>{
              'text': 'hello',
              'settleTimeoutMs': 0,
            }),
          );
          expect(firstCtrl.text, 'hello');

          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('pressKey', <String, Object?>{
              'key': 'BACKSPACE',
              'settleTimeoutMs': 0,
            }),
          );
          expect(firstCtrl.text, 'hell');

          // pressKey TAB moves focus from First to Second!
          expect(firstFocus.hasFocus, isTrue);
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('pressKey', <String, Object?>{
              'key': 'TAB',
              'settleTimeoutMs': 0,
            }),
          );
          await tester.pump();
          expect(secondFocus.hasFocus, isTrue);

          // Focus First again and press ENTER -> triggers onSubmitted("hell").
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('tapOn', <String, Object?>{
              'selector': <String, Object?>{'text': 'hell'},
              'timeoutMs': 500,
              'settleTimeoutMs': 0,
            }),
          );
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('pressKey', <String, Object?>{
              'key': 'ENTER',
              'settleTimeoutMs': 0,
            }),
          );
          await tester.pump();
          expect(submitted, 'hell');
        } finally {
          tester.binding.defaultBinaryMessenger.setMockMessageHandler(
            SystemChannels.textInput.name,
            null,
          );
          firstCtrl.dispose();
          secondCtrl.dispose();
          firstFocus.dispose();
          secondFocus.dispose();
          handle.dispose();
        }
      },
    );

    testWidgets('pressKey ESCAPE and BACK dismiss dialogs at engine boundary', (
      WidgetTester tester,
    ) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        await tester.pumpWidget(
          MaterialApp(
            home: Builder(
              builder: (BuildContext context) => Scaffold(
                body: Center(
                  child: ElevatedButton(
                    onPressed: () {
                      unawaited(
                        showDialog<void>(
                          context: context,
                          builder: (BuildContext ctx) => AlertDialog(
                            title: const Text('Popup Dialog'),
                            actions: <Widget>[
                              TextButton(
                                onPressed: () => Navigator.pop(ctx),
                                child: const Text('Close'),
                              ),
                            ],
                          ),
                        ),
                      );
                    },
                    child: const Text('Open Dialog'),
                  ),
                ),
              ),
            ),
          ),
        );

        final CommandDispatcher dispatcher = CommandDispatcher(
          makeContext(tester),
        );

        await runDriver<Object?>(
          tester,
          dispatcher.dispatch('tapOn', <String, Object?>{
            'selector': <String, Object?>{'text': 'Open Dialog'},
            'timeoutMs': 500,
            'settleTimeoutMs': 0,
          }),
        );
        await tester.pumpAndSettle();
        expect(find.text('Popup Dialog'), findsOneWidget);

        await runDriver<Object?>(
          tester,
          dispatcher.dispatch('pressKey', <String, Object?>{
            'key': 'ESCAPE',
            'settleTimeoutMs': 0,
          }),
        );
        await tester.pumpAndSettle();
        expect(find.text('Popup Dialog'), findsNothing);

        // Verify pressKey BACK delegates to PointerInput.back() (flutter/navigation popRoute).
        int backCalls = 0;
        final CommandDispatcher backDispatcher = CommandDispatcher(
          DriverContext(
            idle: IdleDetector(
              httpTracker: HttpTracker(),
              textInput: TextInputController(),
              readBottomInset: () => 0,
              waitForFirstFrame: (_) async {},
            ),
            pointer: _SpyBackPointerInput(() => backCalls++),
          ),
        );
        await runDriver<Object?>(
          tester,
          backDispatcher.dispatch('pressKey', <String, Object?>{
            'key': 'BACK',
            'settleTimeoutMs': 0,
          }),
        );
        expect(backCalls, 1);
      } finally {
        handle.dispose();
      }
    });

    testWidgets('darkMode RPC reports platformBrightness', (
      WidgetTester tester,
    ) async {
      ui.Brightness current = ui.Brightness.light;
      final CommandDispatcher dispatcher = CommandDispatcher(
        makeContext(tester, readPlatformBrightness: () => current),
      );

      final Map<String, Object?> lightResp =
          (await dispatcher.dispatch('darkMode', const <String, Object?>{}))!
              as Map<String, Object?>;
      expect(lightResp['darkMode'], isFalse);
      expect(lightResp['brightness'], 'light');

      current = ui.Brightness.dark;
      final Map<String, Object?> darkResp =
          (await dispatcher.dispatch('darkMode', const <String, Object?>{}))!
              as Map<String, Object?>;
      expect(darkResp['darkMode'], isTrue);
      expect(darkResp['brightness'], 'dark');
    });

    testWidgets(
      'back and pressKey BACK hide keyboard when insets > 0 instead of popping route; hideKeyboard without keyboard pops route with warning',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          double bottomInset = 600;
          int backCalls = 0;
          final TextInputController textInput = TextInputController();
          textInput.onOutgoing(
            const JSONMethodCodec().encodeMethodCall(
              const MethodCall('TextInput.show'),
            ),
          );
          expect(textInput.keyboardRequested, isTrue);

          final CommandDispatcher dispatcher = CommandDispatcher(
            DriverContext(
              idle: IdleDetector(
                httpTracker: HttpTracker(),
                textInput: textInput,
                readBottomInset: () => bottomInset,
                waitForFirstFrame: (_) async {},
              ),
              pointer: _SpyBackPointerInput(() => backCalls++),
              textInput: textInput,
              readBottomInset: () => bottomInset,
            ),
          );

          // 1. back with keyboard up -> hides keyboard, does NOT popRoute.
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('back', <String, Object?>{
              'settleTimeoutMs': 0,
            }),
          );
          expect(backCalls, 0);
          expect(textInput.keyboardRequested, isFalse);

          // 2. pressKey BACK with keyboard up -> hides keyboard, does NOT popRoute.
          textInput.onOutgoing(
            const JSONMethodCodec().encodeMethodCall(
              const MethodCall('TextInput.show'),
            ),
          );
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('pressKey', <String, Object?>{
              'key': 'BACK',
              'settleTimeoutMs': 0,
            }),
          );
          expect(backCalls, 0);
          expect(textInput.keyboardRequested, isFalse);

          // 3. hideKeyboard with keyboard up -> hides keyboard, NO warning.
          textInput.onOutgoing(
            const JSONMethodCodec().encodeMethodCall(
              const MethodCall('TextInput.show'),
            ),
          );
          final Map<String, Object?> hideUp =
              (await runDriver<Object?>(
                    tester,
                    dispatcher.dispatch('hideKeyboard', <String, Object?>{
                      'settleTimeoutMs': 0,
                    }),
                  ))!
                  as Map<String, Object?>;
          expect(backCalls, 0);
          expect(hideUp.containsKey('warning'), isFalse);

          // 4. With keyboard hidden (insets == 0): back pops route, and
          // hideKeyboard also pops route with a Maestro-quirk warning.
          bottomInset = 0;
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('back', <String, Object?>{
              'settleTimeoutMs': 0,
            }),
          );
          expect(backCalls, 1);

          final Map<String, Object?> hideDown =
              (await runDriver<Object?>(
                    tester,
                    dispatcher.dispatch('hideKeyboard', <String, Object?>{
                      'settleTimeoutMs': 0,
                    }),
                  ))!
                  as Map<String, Object?>;
          expect(backCalls, 2);
          expect(hideDown['warning'], isA<String>());
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets(
      'scrollUntilVisible advances snapping PageView via Maestro-equivalent fling velocity',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: PageView(
                  children: const <Widget>[
                    Center(child: Text('Snap Page 1')),
                    Center(child: Text('Snap Page 2')),
                    Center(child: Text('Snap Page 3')),
                  ],
                ),
              ),
            ),
          );

          final CommandDispatcher dispatcher = CommandDispatcher(
            makeContext(tester),
          );
          final Map<String, Object?> result =
              (await runDriver<Object?>(
                    tester,
                    dispatcher.dispatch('scrollUntilVisible', <String, Object?>{
                      'selector': <String, Object?>{'text': 'Snap Page 3'},
                      'direction': 'RIGHT',
                      'durationMs': 601,
                      'timeoutMs': 10000,
                      'settleTimeoutMs': 500,
                    }),
                  ))!
                  as Map<String, Object?>;
          expect(result['swipes']! as int, greaterThanOrEqualTo(2));
        } finally {
          handle.dispose();
        }
      },
    );

    testWidgets(
      'accelerated virtual-timestamp drag produces expected VelocityTracker velocity and high-water clock catches up',
      (WidgetTester tester) async {
        final DateTime epoch = tester.binding.clock.now();
        double? measuredVelocityPxPerSec;

        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: GestureDetector(
                behavior: HitTestBehavior.opaque,
                onVerticalDragEnd: (DragEndDetails details) {
                  measuredVelocityPxPerSec =
                      details.velocity.pixelsPerSecond.dy;
                },
                child: const SizedBox.expand(),
              ),
            ),
          ),
        );

        final double dpr = tester.view.devicePixelRatio;
        final ui.Size phys = tester.view.physicalSize;
        final PointerInput pointer = PointerInput(
          clock: () => tester.binding.clock.now().difference(epoch),
        );

        // Drag 40% of screen height upward over logical 601 ms in 24 ms wall time.
        final ui.Offset start = ui.Offset(phys.width * 0.5, phys.height * 0.5);
        final ui.Offset end = ui.Offset(phys.width * 0.5, phys.height * 0.1);
        await runDriver<void>(
          tester,
          pointer.drag(
            start,
            end,
            const Duration(milliseconds: 601),
            wallDuration: const Duration(milliseconds: 24),
          ),
        );

        final double expectedLogicalVel = -((start.dy - end.dy) / dpr) / 0.601;
        expect(measuredVelocityPxPerSec, isNotNull);
        expect(
          measuredVelocityPxPerSec!,
          closeTo(expectedLogicalVel, expectedLogicalVel.abs() * 0.15),
        );
      },
    );

    testWidgets(
      'retryTapIfNoChange respects kDoubleTapTimeout and does not trigger onDoubleTap',
      (WidgetTester tester) async {
        final SemanticsHandle handle = tester.ensureSemantics();
        try {
          int taps = 0;
          int doubleTaps = 0;
          await tester.pumpWidget(
            MaterialApp(
              home: Scaffold(
                body: Center(
                  child: GestureDetector(
                    onTap: () => taps++,
                    onDoubleTap: () => doubleTaps++,
                    child: const Padding(
                      padding: EdgeInsets.all(24),
                      child: Text('Dual Gesture'),
                    ),
                  ),
                ),
              ),
            ),
          );

          final CommandDispatcher dispatcher = CommandDispatcher(
            makeContext(tester),
          );
          await runDriver<Object?>(
            tester,
            dispatcher.dispatch('tapOn', <String, Object?>{
              'selector': <String, Object?>{'text': 'Dual Gesture'},
              'retryIfNoChange': true,
              'timeoutMs': 500,
              'settleTimeoutMs': 100,
            }),
          );
          // Drain final double-tap arena timer.
          await tester.pump(const Duration(milliseconds: 350));
          expect(doubleTaps, 0);
          expect(taps, 2);
        } finally {
          handle.dispose();
        }
      },
    );
  });
}

class _SpyBackPointerInput extends PointerInput {
  _SpyBackPointerInput(this._onBack);

  final VoidCallback _onBack;

  @override
  void back() {
    _onBack();
  }
}
