import 'dart:async';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/platform.dart';
import 'package:tapwing_driver/src/protocol/rpc_error.dart';
import 'package:tapwing_driver/src/selector/selector.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';
import 'package:tapwing_driver/src/server/tapwing_server.dart';
import 'package:tapwing_driver/src/snapshot/snapshot.dart';

/// iPhone 18 Pro simulator geometry: 402x874 pt @3x.
const ui.Size _iphonePhysical = ui.Size(1206, 2622);

void main() {
  void useIphoneView(WidgetTester tester) {
    tester.view.physicalSize = _iphonePhysical;
    tester.view.devicePixelRatio = 3.0;
    addTearDown(tester.view.reset);
  }

  ({CommandDispatcher dispatcher, DriverContext context}) makeDriver(
    WidgetTester tester, {
    DriverPlatform platform = DriverPlatform.ios,
    TextInputController? textInput,
    PointerInput? pointer,
    double Function()? readBottomInset,
    bool strictHideKeyboard = false,
    String? appName,
  }) {
    final DateTime epoch = tester.binding.clock.now();
    final TextInputController ti = textInput ?? TextInputController();
    final DriverContext context = DriverContext(
      idle: IdleDetector(
        httpTracker: HttpTracker(),
        textInput: ti,
        readBottomInset: readBottomInset ?? () => 0,
        waitForFirstFrame: (_) async {},
      ),
      pointer:
          pointer ??
          PointerInput(
            clock: () => tester.binding.clock.now().difference(epoch),
          ),
      textInput: ti,
      readBottomInset: readBottomInset ?? () => 0,
      platform: platform,
      strictHideKeyboard: strictHideKeyboard,
      appName: appName,
    );
    return (dispatcher: CommandDispatcher(context), context: context);
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

  /// Mirrors framework `flutter/textinput` calls into [textInput], like the
  /// binding's `ObservingBinaryMessenger` does in a real app.
  void observeTextInput(WidgetTester tester, TextInputController textInput) {
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.textInput,
      (MethodCall call) async {
        textInput.onOutgoing(const JSONMethodCodec().encodeMethodCall(call));
        return null;
      },
    );
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.textInput,
        null,
      ),
    );
  }

  group('iOS hierarchy composition (IOSDriver.mapViewHierarchy)', () {
    testWidgets('bounds are whole points, app root element, Maestro iOS '
        'attributes', (WidgetTester tester) async {
      useIphoneView(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      final TextEditingController email = TextEditingController(
        text: 'jan@example.com',
      );
      addTearDown(email.dispose);
      try {
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: Stack(
                children: <Widget>[
                  Positioned(
                    left: 24,
                    top: 194,
                    width: 354,
                    height: 56,
                    child: Semantics(
                      identifier: 'email_input',
                      child: TextField(
                        controller: email,
                        decoration: const InputDecoration(
                          labelText: 'Email',
                          hintText: 'you@domain.com',
                        ),
                      ),
                    ),
                  ),
                  Positioned(
                    left: 16,
                    top: 300,
                    width: 370,
                    height: 56,
                    child: Semantics(
                      identifier: 'fav',
                      label: 'Favorite',
                      toggled: true,
                      enabled: true,
                      child: const SizedBox.expand(),
                    ),
                  ),
                  Positioned(
                    left: 16,
                    top: 400,
                    width: 100,
                    height: 40,
                    child: Semantics(
                      label: 'Agree',
                      checked: false,
                      enabled: true,
                      child: const SizedBox.expand(),
                    ),
                  ),
                  Positioned(
                    left: 16,
                    top: 460,
                    width: 100,
                    height: 40,
                    child: Semantics(
                      label: 'Option A',
                      checked: true,
                      inMutuallyExclusiveGroup: true,
                      child: const SizedBox.expand(),
                    ),
                  ),
                  Positioned(
                    left: 16,
                    top: 520,
                    width: 100,
                    height: 40,
                    child: Semantics(
                      label: 'Volume',
                      value: '50%',
                      hint: 'Swipe to adjust',
                      tooltip: 'Media volume',
                      button: true,
                      onTap: () {},
                      child: const SizedBox.expand(),
                    ),
                  ),
                ],
              ),
            ),
          ),
        );

        final Snapshot snap = Snapshot.capture(
          platform: DriverPlatform.ios,
          appName: 'Sample App',
        );
        expect(snap.screen, const ui.Size(402, 874));
        // XCUIApplication element first, labelled with the display name.
        final KNode app = snap.root!;
        expect(app.accessibilityText, 'Sample App');
        expect(app.bounds, const Rect.fromLTRB(0, 0, 402, 874));
        expect(snap.nodes.first, same(app));

        final KNode field = snap.nodes.firstWhere(
          (KNode n) => n.resourceId == 'email_input',
        );
        expect(field.bounds, const Rect.fromLTRB(24, 194, 378, 250));
        expect(field.accessibilityText, 'Email');
        expect(field.text, 'jan@example.com');
        // Hints become accessibilityHint on iOS, never placeholderValue.
        expect(field.hintText, '');
        expect(field.focused, isFalse);
        expect(field.clickable, isFalse);
        expect(field.isDescendantOf(app), isTrue);

        final KNode fav = snap.nodes.firstWhere(
          (KNode n) => n.resourceId == 'fav',
        );
        expect(fav.text, '1');
        expect(fav.checked, isTrue);
        expect(fav.bounds, const Rect.fromLTRB(16, 300, 386, 356));

        final KNode agree = snap.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Agree',
        );
        expect(agree.text, '0');
        expect(agree.checked, isFalse);

        // Radio buttons are plain elements: no value, never `checked`.
        final KNode radio = snap.nodes.firstWhere(
          (KNode n) => n.accessibilityText == 'Option A',
        );
        expect(radio.text, '');
        expect(radio.checked, isFalse);

        final KNode volume = snap.nodes.firstWhere(
          (KNode n) => (n.accessibilityText ?? '').startsWith('Volume'),
        );
        expect(volume.accessibilityText, 'Volume\nMedia volume');
        expect(volume.text, '50%');
        expect(volume.clickable, isFalse);

        // JSON omits empty iOS attributes and `clickable`.
        final Map<String, Object?> json = volume.toJson();
        expect(json.containsKey('clickable'), isFalse);
        expect(json.containsKey('hintText'), isFalse);
        expect(json['bounds'], <int>[16, 520, 116, 560]);

        // Android composition of the same tree is unchanged (physical px).
        final Snapshot android = Snapshot.capture();
        final KNode aField = android.nodes.firstWhere(
          (KNode n) => n.resourceId == 'email_input',
        );
        expect(aField.bounds, const Rect.fromLTRB(72, 582, 1134, 750));
        expect(aField.hintText, 'Email');
        expect(android.nodes.any((KNode n) => n.id == -1), isFalse);
      } finally {
        handle.dispose();
      }
    });
  });

  group('iOS point coordinates', () {
    testWidgets('selectors, point parsing and taps use points while pointer '
        'packets stay physical', (WidgetTester tester) async {
      useIphoneView(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      final List<Offset> downs = <Offset>[];
      try {
        await tester.pumpWidget(
          MaterialApp(
            home: Listener(
              behavior: HitTestBehavior.opaque,
              onPointerDown: (PointerDownEvent e) => downs.add(e.position),
              child: Stack(
                children: <Widget>[
                  Positioned(
                    left: 50,
                    top: 100,
                    width: 100,
                    height: 50,
                    child: Semantics(
                      identifier: 'box',
                      container: true,
                      child: const SizedBox.expand(),
                    ),
                  ),
                ],
              ),
            ),
          ),
        );
        final ({CommandDispatcher dispatcher, DriverContext context}) d =
            makeDriver(tester);
        expect(d.context.pixelsPerUnit, 3.0);
        expect(d.context.readScreenSize(), const ui.Size(402, 874));
        expect(d.context.parsePoint('50%, 50%'), const Offset(201, 437));

        // `width`/`height` are points on iOS (Maestro iOS bounds).
        final Snapshot snap = d.context.captureSnapshot();
        expect(
          resolve(
            Selector.fromJson(<String, Object?>{'width': 100, 'height': 50}),
            snap,
          )?.resourceId,
          'box',
        );
        expect(
          resolve(
            Selector.fromJson(<String, Object?>{'width': 300, 'height': 150}),
            snap,
          ),
          isNull,
        );

        // Absolute points land at the same logical position.
        await runDriver<Object?>(
          tester,
          d.dispatcher.dispatch('tapPoint', <String, Object?>{
            'point': '100, 125',
            'settleTimeoutMs': 0,
          }),
        );
        expect(downs.last, const Offset(100, 125));

        // Element taps hit the element center.
        await runDriver<Object?>(
          tester,
          d.dispatcher.dispatch('tapOn', <String, Object?>{
            'selector': <String, Object?>{'id': 'box'},
            'timeoutMs': 1000,
            'settleTimeoutMs': 0,
          }),
        );
        expect(downs.last, const Offset(100, 125));

        // The same numbers on Android are physical pixels.
        final ({CommandDispatcher dispatcher, DriverContext context}) a =
            makeDriver(tester, platform: DriverPlatform.android);
        expect(a.context.pixelsPerUnit, 1.0);
        await runDriver<Object?>(
          tester,
          a.dispatcher.dispatch('tapPoint', <String, Object?>{
            'point': '300, 375',
            'settleTimeoutMs': 0,
          }),
        );
        expect(downs.last, const Offset(100, 125));
      } finally {
        handle.dispose();
      }
    });

    testWidgets('swipe coordinates are points (scroll distance matches)', (
      WidgetTester tester,
    ) async {
      useIphoneView(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      final ScrollController controller = ScrollController();
      addTearDown(controller.dispose);
      try {
        await tester.pumpWidget(
          MaterialApp(
            home: ListView.builder(
              controller: controller,
              physics: const ClampingScrollPhysics(),
              itemCount: 200,
              itemBuilder: (_, int i) =>
                  SizedBox(height: 50, child: Text('Row $i')),
            ),
          ),
        );
        final ({CommandDispatcher dispatcher, DriverContext context}) d =
            makeDriver(tester);
        // A slow 2 s drag of 200 pt leaves no fling: offset ~= 200 pt minus
        // the touch slop consumed before the drag starts.
        await runDriver<Object?>(
          tester,
          d.dispatcher.dispatch('swipe', <String, Object?>{
            'start': '200, 600',
            'end': '200, 400',
            'durationMs': 2000,
            'settleTimeoutMs': 0,
          }),
        );
        await tester.pumpAndSettle();
        expect(controller.offset, inInclusiveRange(170, 200));
      } finally {
        handle.dispose();
      }
    });
  });

  group('iOS command semantics', () {
    testWidgets('back is a no-op and pressKey ignores keys IOSDriver does not '
        'map', (WidgetTester tester) async {
      useIphoneView(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      int backCalls = 0;
      try {
        await tester.pumpWidget(const MaterialApp(home: Text('Home')));
        final ({CommandDispatcher dispatcher, DriverContext context}) d =
            makeDriver(tester, pointer: _SpyPointer(() => backCalls++));
        final Map<String, Object?> back =
            (await runDriver<Object?>(
                  tester,
                  d.dispatcher.dispatch('back', <String, Object?>{}),
                ))!
                as Map<String, Object?>;
        expect(back['noop'], isTrue);
        for (final String key in <String>['BACK', 'TAB', 'ESCAPE']) {
          final Map<String, Object?> res =
              (await runDriver<Object?>(
                    tester,
                    d.dispatcher.dispatch('pressKey', <String, Object?>{
                      'key': key,
                    }),
                  ))!
                  as Map<String, Object?>;
          expect(res['noop'], isTrue, reason: key);
        }
        expect(backCalls, 0);
      } finally {
        handle.dispose();
      }
    });

    Widget loginForm({required bool unfocusOnTapOutside}) {
      Widget app = const Scaffold(
        body: Padding(
          padding: EdgeInsets.only(top: 100),
          child: Column(
            children: <Widget>[
              TextField(decoration: InputDecoration(labelText: 'Email')),
            ],
          ),
        ),
      );
      if (unfocusOnTapOutside) {
        app = Actions(
          actions: <Type, Action<Intent>>{
            EditableTextTapOutsideIntent:
                CallbackAction<EditableTextTapOutsideIntent>(
                  onInvoke: (EditableTextTapOutsideIntent intent) {
                    intent.focusNode.unfocus();
                    return null;
                  },
                ),
          },
          child: app,
        );
      }
      return MaterialApp(home: app);
    }

    testWidgets('hideKeyboard default: hides, keeps focus and warns; no '
        'Android back fallback', (WidgetTester tester) async {
      debugDefaultTargetPlatformOverride = TargetPlatform.iOS;
      useIphoneView(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      int backCalls = 0;
      try {
        final TextInputController textInput = TextInputController();
        observeTextInput(tester, textInput);
        await tester.pumpWidget(loginForm(unfocusOnTapOutside: false));
        final ({CommandDispatcher dispatcher, DriverContext context}) d =
            makeDriver(
              tester,
              textInput: textInput,
              pointer: _SpyPointer(() => backCalls++),
            );

        // No focused field: succeeds silently (Maestro iOS: keyboard hidden).
        final Map<String, Object?> idle =
            (await runDriver<Object?>(
                  tester,
                  d.dispatcher.dispatch('hideKeyboard', <String, Object?>{
                    'settleTimeoutMs': 0,
                  }),
                ))!
                as Map<String, Object?>;
        expect(idle.containsKey('warning'), isFalse);
        expect(backCalls, 0);

        await tester.tap(find.byType(TextField));
        await tester.pump();
        expect(textInput.hasClient, isTrue);
        expect(textInput.keyboardRequested, isTrue);

        final Map<String, Object?> res =
            (await runDriver<Object?>(
                  tester,
                  d.dispatcher.dispatch('hideKeyboard', <String, Object?>{
                    'settleTimeoutMs': 0,
                  }),
                ))!
                as Map<String, Object?>;
        expect(res['warning'], CommandDispatcher.iosHideKeyboardFocusWarning);
        expect(textInput.keyboardRequested, isFalse);
        expect(textInput.hasClient, isTrue);
        expect(backCalls, 0);
      } finally {
        handle.dispose();
        debugDefaultTargetPlatformOverride = null;
      }
    });

    testWidgets('hideKeyboard strict: fails like Maestro iOS when the field '
        'stays focused', (WidgetTester tester) async {
      debugDefaultTargetPlatformOverride = TargetPlatform.iOS;
      useIphoneView(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        final TextInputController textInput = TextInputController();
        observeTextInput(tester, textInput);
        await tester.pumpWidget(loginForm(unfocusOnTapOutside: false));
        final ({CommandDispatcher dispatcher, DriverContext context}) d =
            makeDriver(tester, textInput: textInput, strictHideKeyboard: true);
        await tester.tap(find.byType(TextField));
        await tester.pump();
        expect(textInput.hasClient, isTrue);

        Object? error;
        try {
          await runDriver<Object?>(
            tester,
            d.dispatcher.dispatch('hideKeyboard', <String, Object?>{
              'settleTimeoutMs': 0,
            }),
          );
        } on DriverError catch (e) {
          error = e;
        }
        expect(
          error,
          isA<DriverError>()
              .having((DriverError e) => e.code, 'code', 'ASSERTION')
              .having(
                (DriverError e) => e.message,
                'message',
                startsWith("Couldn't hide the keyboard."),
              ),
        );
        expect(textInput.hasClient, isTrue);
      } finally {
        handle.dispose();
        debugDefaultTargetPlatformOverride = null;
      }
    });

    testWidgets('hideKeyboard strict: micro-swipes unfocus when the app '
        'unfocuses on tap outside', (WidgetTester tester) async {
      debugDefaultTargetPlatformOverride = TargetPlatform.iOS;
      useIphoneView(tester);
      final SemanticsHandle handle = tester.ensureSemantics();
      try {
        final TextInputController textInput = TextInputController();
        observeTextInput(tester, textInput);
        await tester.pumpWidget(loginForm(unfocusOnTapOutside: true));
        final ({CommandDispatcher dispatcher, DriverContext context}) d =
            makeDriver(tester, textInput: textInput, strictHideKeyboard: true);
        await tester.tap(find.byType(TextField));
        await tester.pump();
        expect(textInput.hasClient, isTrue);

        final Map<String, Object?> res =
            (await runDriver<Object?>(
                  tester,
                  d.dispatcher.dispatch('hideKeyboard', <String, Object?>{
                    'settleTimeoutMs': 0,
                  }),
                ))!
                as Map<String, Object?>;
        expect(res.containsKey('warning'), isFalse);
        expect(textInput.hasClient, isFalse);
      } finally {
        handle.dispose();
        debugDefaultTargetPlatformOverride = null;
      }
    });

    testWidgets('configure switches platform, app name and strict mode', (
      WidgetTester tester,
    ) async {
      useIphoneView(tester);
      final ({CommandDispatcher dispatcher, DriverContext context}) d =
          makeDriver(tester, platform: DriverPlatform.android);
      final Map<String, Object?> res =
          (await d.dispatcher.dispatch('configure', <String, Object?>{
                'platform': 'ios',
                'appName': 'Sample App',
                'strictHideKeyboard': true,
              }))!
              as Map<String, Object?>;
      expect(res['platform'], 'ios');
      expect(res['appName'], 'Sample App');
      expect(res['strictHideKeyboard'], isTrue);
      expect(res['pixelsPerUnit'], 3.0);
      expect(d.context.usesPoints, isTrue);

      await d.dispatcher.dispatch('configure', <String, Object?>{
        'platform': 'android',
        'appName': '',
      });
      expect(d.context.platform, DriverPlatform.android);
      expect(d.context.appName, isNull);
      expect(d.context.pixelsPerUnit, 1.0);
      expect(
        () => d.dispatcher.dispatch('configure', <String, Object?>{
          'platform': 'web',
        }),
        throwsA(isA<DriverError>()),
      );
    });
  });

  group('port handoff', () {
    test('environment, then tmp/tapwing_port, then the default port', () {
      final Directory tmp = Directory.systemTemp.createTempSync('tapwing');
      addTearDown(() => tmp.deleteSync(recursive: true));
      expect(
        TapwingServer.resolvePort(
          environment: const <String, String>{},
          tempDir: tmp,
        ),
        defaultDriverPort,
      );
      File('${tmp.path}/tapwing_port').writeAsStringSync('54321\n');
      expect(
        TapwingServer.resolvePort(
          environment: const <String, String>{},
          tempDir: tmp,
        ),
        54321,
      );
      expect(
        TapwingServer.resolvePort(
          environment: const <String, String>{'TAPWING_PORT': '40000'},
          tempDir: tmp,
        ),
        40000,
      );
      File('${tmp.path}/tapwing_port').writeAsStringSync('not a port');
      expect(
        TapwingServer.resolvePort(
          environment: const <String, String>{},
          tempDir: tmp,
        ),
        defaultDriverPort,
      );
    });
  });
}

class _SpyPointer extends PointerInput {
  _SpyPointer(this._onBack);

  final VoidCallback _onBack;

  @override
  void back() => _onBack();
}
