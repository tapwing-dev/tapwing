import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:sample_app/main.dart';

/// Pumps the login screen past the first frame on a phone-sized surface
/// (360x1400 logical pixels), tall enough for every Playground hub entry.
Future<void> _pumpApp(WidgetTester tester) async {
  tester.view
    ..physicalSize = const Size(1080, 4200)
    ..devicePixelRatio = 3;
  addTearDown(tester.view.reset);
  await tester.pumpWidget(const SampleApp());
  await tester.pumpAndSettle();
}

Future<void> _login(WidgetTester tester) async {
  await tester.enterText(
    find.widgetWithText(TextField, 'Email'),
    'jan@example.com',
  );
  await tester.enterText(
    find.widgetWithText(TextField, 'Password'),
    'secret123',
  );
  await tester.tap(find.widgetWithText(FilledButton, 'Log in'));
  // The simulated backend call takes 600 ms.
  await tester.pump(const Duration(milliseconds: 700));
  await tester.pumpAndSettle();
}

/// An [AppStateStore] in memory, standing in for the platform preferences and
/// the documents directory (`clearState` is a device-level concern).
class _MemoryStore implements AppStateStore {
  String? preference;
  String? file;

  @override
  Future<String?> readPreference() async => preference;

  @override
  Future<void> writePreference(String value) async => preference = value;

  @override
  Future<String?> readFile() async => file;

  @override
  Future<void> writeFile(String value) async => file = value;
}

void main() {
  group('Login screen', () {
    testWidgets('shows the welcome text, both fields and both buttons', (
      WidgetTester tester,
    ) async {
      await _pumpApp(tester);

      expect(find.text('Welcome back'), findsOneWidget);
      expect(find.widgetWithText(TextField, 'Email'), findsOneWidget);
      expect(find.widgetWithText(TextField, 'Password'), findsOneWidget);
      expect(find.widgetWithText(FilledButton, 'Log in'), findsOneWidget);
      expect(find.widgetWithText(TextButton, 'F2 Playground'), findsOneWidget);
      expect(find.widgetWithText(TextButton, 'Browser login'), findsOneWidget);
      expect(find.text('Email is required'), findsNothing);
    });

    testWidgets('the password field hides its text', (
      WidgetTester tester,
    ) async {
      await _pumpApp(tester);

      final TextField password = tester.widget<TextField>(
        find.widgetWithText(TextField, 'Password'),
      );
      expect(password.obscureText, isTrue);
      final TextField email = tester.widget<TextField>(
        find.widgetWithText(TextField, 'Email'),
      );
      expect(email.obscureText, isFalse);
      expect(email.keyboardType, TextInputType.emailAddress);
    });

    testWidgets('logging in without an email shows an error and stays put', (
      WidgetTester tester,
    ) async {
      await _pumpApp(tester);

      await tester.tap(find.widgetWithText(FilledButton, 'Log in'));
      await tester.pump();

      expect(find.text('Email is required'), findsOneWidget);
      expect(find.text('Welcome back'), findsOneWidget);
      expect(find.text('Products'), findsNothing);
    });

    testWidgets(
      'a valid login shows a spinner, disables the button, then opens Products',
      (WidgetTester tester) async {
        await _pumpApp(tester);
        await tester.enterText(
          find.widgetWithText(TextField, 'Email'),
          'jan@example.com',
        );
        await tester.tap(find.widgetWithText(FilledButton, 'Log in'));
        await tester.pump(const Duration(milliseconds: 100));

        expect(find.byType(CircularProgressIndicator), findsOneWidget);
        final FilledButton busy = tester.widget<FilledButton>(
          find.byType(FilledButton),
        );
        expect(
          busy.onPressed,
          isNull,
          reason: 'no double submit while loading',
        );

        await tester.pump(const Duration(milliseconds: 600));
        await tester.pumpAndSettle();
        expect(find.text('Products'), findsOneWidget);
        expect(find.text('Welcome back'), findsNothing);
      },
    );

    testWidgets('a previous error disappears after a successful retry', (
      WidgetTester tester,
    ) async {
      await _pumpApp(tester);
      await tester.tap(find.widgetWithText(FilledButton, 'Log in'));
      await tester.pump();
      expect(find.text('Email is required'), findsOneWidget);

      await _login(tester);
      expect(find.text('Email is required'), findsNothing);
      expect(find.text('Products'), findsOneWidget);
    });
  });

  group('Products and detail', () {
    testWidgets('lists products lazily and replaces the login route', (
      WidgetTester tester,
    ) async {
      await _pumpApp(tester);
      await _login(tester);

      expect(find.text('Product 1'), findsOneWidget);
      expect(find.text('€ 3.00'), findsOneWidget);
      expect(find.text('Product 40'), findsNothing, reason: 'built lazily');
      // pushReplacement: there is nothing to go back to.
      expect(find.byType(BackButton), findsNothing);
    });

    testWidgets('scrolling reaches a later product and opens its detail', (
      WidgetTester tester,
    ) async {
      await _pumpApp(tester);
      await _login(tester);

      await tester.scrollUntilVisible(
        find.text('Product 40'),
        300,
        scrollable: find.byType(Scrollable).first,
      );
      await tester.ensureVisible(find.text('Product 40'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Product 40'));
      await tester.pumpAndSettle();

      expect(find.text('Details of Product 40'), findsOneWidget);
      expect(find.widgetWithText(AppBar, 'Product 40'), findsOneWidget);
    });

    testWidgets('the favorite switch toggles and back returns to the list', (
      WidgetTester tester,
    ) async {
      await _pumpApp(tester);
      await _login(tester);
      await tester.tap(find.text('Product 1'));
      await tester.pumpAndSettle();

      Switch favorite() => tester.widget<Switch>(find.byType(Switch));
      expect(favorite().value, isFalse);
      await tester.tap(find.byType(Switch));
      await tester.pump();
      expect(favorite().value, isTrue);
      await tester.tap(find.byType(Switch));
      await tester.pump();
      expect(favorite().value, isFalse);

      await tester.pageBack();
      await tester.pumpAndSettle();
      expect(find.text('Products'), findsOneWidget);
    });

    testWidgets(
      'the add dialog confirms with a snackbar and Cancel adds nothing',
      (WidgetTester tester) async {
        await _pumpApp(tester);
        await _login(tester);

        await tester.tap(find.byTooltip('Add product'));
        await tester.pumpAndSettle();
        expect(find.text('New product'), findsOneWidget);
        await tester.tap(find.text('Cancel'));
        await tester.pumpAndSettle();
        expect(find.text('New product'), findsNothing);
        expect(find.byType(SnackBar), findsNothing);

        await tester.tap(find.byTooltip('Add product'));
        await tester.pumpAndSettle();
        await tester.enterText(
          find.widgetWithText(TextField, 'Name'),
          'Tapwing',
        );
        await tester.tap(find.text('Add'));
        await tester.pumpAndSettle();
        expect(find.text('Added Tapwing'), findsOneWidget);
        expect(find.text('New product'), findsNothing);
      },
    );

    testWidgets('adding an empty name shows no snackbar', (
      WidgetTester tester,
    ) async {
      await _pumpApp(tester);
      await _login(tester);
      await tester.tap(find.byTooltip('Add product'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('Add'));
      await tester.pumpAndSettle();
      expect(find.byType(SnackBar), findsNothing);
    });
  });

  group('F2 Playground', () {
    testWidgets('opens from the login screen and lists every hub entry', (
      WidgetTester tester,
    ) async {
      await _pumpApp(tester);
      await tester.tap(find.text('F2 Playground'));
      await tester.pumpAndSettle();

      for (final String entry in <String>[
        'Selectors & Taps',
        'Swipe & Scroll',
        'Text & Keys',
        'Screenshot & Theme',
        'Endless Animation',
        'Request Location Permission',
        'Async Channel Nav',
        'Network',
        'App Health',
        'App State',
      ]) {
        expect(find.text(entry), findsOneWidget, reason: entry);
      }
      expect(find.text('Permission: unknown'), findsOneWidget);
    });

    for (final (String, Future<Object?> Function(MethodCall), String) c
        in <(String, Future<Object?> Function(MethodCall), String)>[
          ('granted', (MethodCall call) async => true, 'Permission: granted'),
          ('denied', (MethodCall call) async => false, 'Permission: denied'),
          (
            'a platform error',
            (MethodCall call) async =>
                throw PlatformException(code: 'DENIED', message: 'no'),
            'Permission: denied',
          ),
        ]) {
      testWidgets('requesting the location permission shows ${c.$1}', (
        WidgetTester tester,
      ) async {
        const MethodChannel channel = MethodChannel(
          'dev.tapwing.sample_app/permissions',
        );
        tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          channel,
          (MethodCall call) {
            expect(call.method, 'requestLocation');
            return c.$2(call);
          },
        );
        addTearDown(
          () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
            channel,
            null,
          ),
        );
        await _pumpApp(tester);
        await tester.tap(find.text('F2 Playground'));
        await tester.pumpAndSettle();

        await tester.tap(find.text('Request Location Permission'));
        await tester.pumpAndSettle();
        expect(find.text(c.$3), findsOneWidget);
        expect(find.text('Permission: unknown'), findsNothing);
      });
    }

    testWidgets(
      'Async Channel Nav awaits method channel reply and then navigates',
      (WidgetTester tester) async {
        const MethodChannel channel = MethodChannel(
          'dev.tapwing.sample_app/async',
        );
        tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          channel,
          (MethodCall call) async {
            expect(call.method, 'fetchAndNavigate');
            return 'ready';
          },
        );
        addTearDown(
          () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
            channel,
            null,
          ),
        );

        await _pumpApp(tester);
        await tester.tap(find.text('F2 Playground'));
        await tester.pumpAndSettle();

        await tester.tap(find.text('Async Channel Nav'));
        await tester.pumpAndSettle();
        expect(find.text('Async Channel Done'), findsOneWidget);
        expect(find.text('Channel result: ready'), findsOneWidget);
      },
    );

    testWidgets(
      'Selectors & Taps handles taps, double taps, long presses and the retry button',
      (WidgetTester tester) async {
        await _pumpApp(tester);
        await tester.tap(find.text('F2 Playground'));
        await tester.pumpAndSettle();
        await tester.tap(find.text('Selectors & Taps'));
        await tester.pumpAndSettle();

        expect(find.text('Traits Ready'), findsOneWidget);
        expect(find.text('Split: none'), findsOneWidget);
        await tester.tap(find.text('Left'));
        await tester.pump();
        expect(find.text('Left hit'), findsOneWidget);
        await tester.tap(find.text('Right'));
        await tester.pump();
        expect(find.text('Right hit'), findsOneWidget);

        await tester.longPress(find.text('Long press me'));
        await tester.pump();
        expect(find.text('Long pressed'), findsOneWidget);

        final Finder double = find.text('Double tap me');
        await tester.tap(double);
        await tester.pump(const Duration(milliseconds: 40));
        await tester.tap(double);
        await tester.pump(const Duration(milliseconds: 400));
        expect(find.text('Double tapped'), findsOneWidget);

        // The first tap changes nothing, the second one succeeds.
        await tester.ensureVisible(find.text('Tap twice for change'));
        await tester.tap(find.text('Tap twice for change'));
        await tester.pump();
        expect(find.text('Retry succeeded'), findsNothing);
        await tester.tap(find.text('Tap twice for change'));
        await tester.pump();
        expect(find.text('Retry succeeded'), findsOneWidget);

        // Leave no pending timers behind.
        await tester.pageBack();
        await tester.pumpAndSettle();
      },
    );

    testWidgets(
      'back navigation returns from the playground to the login screen',
      (WidgetTester tester) async {
        await _pumpApp(tester);
        await tester.tap(find.text('F2 Playground'));
        await tester.pumpAndSettle();
        await tester.pageBack();
        await tester.pumpAndSettle();
        expect(find.text('Welcome back'), findsOneWidget);
      },
    );
  });

  group('Theming', () {
    testWidgets('follows the platform brightness (light and dark)', (
      WidgetTester tester,
    ) async {
      tester.platformDispatcher.platformBrightnessTestValue = Brightness.dark;
      addTearDown(tester.platformDispatcher.clearPlatformBrightnessTestValue);
      await _pumpApp(tester);
      BuildContext ctx = tester.element(find.text('Welcome back'));
      expect(Theme.of(ctx).brightness, Brightness.dark);

      tester.platformDispatcher.platformBrightnessTestValue = Brightness.light;
      await tester.pumpAndSettle();
      ctx = tester.element(find.text('Welcome back'));
      expect(Theme.of(ctx).brightness, Brightness.light);
    });

    testWidgets('the app is titled for the task switcher', (
      WidgetTester tester,
    ) async {
      await _pumpApp(tester);
      expect(
        tester.widget<MaterialApp>(find.byType(MaterialApp)).title,
        'Tapwing Sample',
      );
    });
  });

  group('App State', () {
    late AppStateStore original;
    late _MemoryStore store;

    setUp(() {
      original = appStateStore;
      store = _MemoryStore();
      appStateStore = store;
    });
    tearDown(() => appStateStore = original);

    Future<void> openAppState(WidgetTester tester) async {
      // A process start: nothing of the previous app tree is left.
      await tester.pumpWidget(const SizedBox.shrink());
      await _pumpApp(tester);
      await tester.tap(find.text('F2 Playground'));
      await tester.pumpAndSettle();
      await tester.tap(find.text('App State'));
      await tester.pumpAndSettle();
    }

    testWidgets('shows a preference and a file that start empty', (
      WidgetTester tester,
    ) async {
      await openAppState(tester);

      expect(find.text('Preference: none'), findsOneWidget);
      expect(find.text('File: none'), findsOneWidget);
    });

    testWidgets('writes both, and a relaunch sees them until the state is cleared', (
      WidgetTester tester,
    ) async {
      await openAppState(tester);
      await tester.tap(find.text('Write state'));
      await tester.pumpAndSettle();

      expect(find.text('Preference: saved-pref'), findsOneWidget);
      expect(find.text('File: saved-file'), findsOneWidget);
      expect(store.preference, 'saved-pref');
      expect(store.file, 'saved-file');

      // A relaunch with the same store (no clearState) still shows them.
      await openAppState(tester);
      expect(find.text('Preference: saved-pref'), findsOneWidget);
      expect(find.text('File: saved-file'), findsOneWidget);

      // What `clearState: true` does to the device, then a relaunch.
      store
        ..preference = null
        ..file = null;
      await openAppState(tester);
      expect(find.text('Preference: none'), findsOneWidget);
      expect(find.text('File: none'), findsOneWidget);
    });
  });

  group('Browser login', () {
    late BrowserAuthenticate original;
    final List<Uri> opened = <Uri>[];

    setUp(() {
      original = browserAuthenticate;
      opened.clear();
    });
    tearDown(() => browserAuthenticate = original);

    /// Stands in for the browser: records the URL and answers with [reply].
    void fakeBrowser(FutureOr<String> Function(Uri url) reply) {
      browserAuthenticate = (Uri url, String callbackScheme) async {
        expect(callbackScheme, webLoginCallbackScheme);
        opened.add(url);
        return reply(url);
      };
    }

    Future<void> openBrowserLogin(WidgetTester tester) async {
      await _pumpApp(tester);
      await tester.tap(find.text('Browser login'));
      await tester.pumpAndSettle();
    }

    testWidgets('opens from the login screen with one button per page variant', (
      WidgetTester tester,
    ) async {
      await openBrowserLogin(tester);

      expect(find.text('Not signed in'), findsOneWidget);
      for (final String variant in <String>['plain', 'two-step', 'controlled']) {
        expect(
          find.widgetWithText(FilledButton, 'Sign in ($variant)'),
          findsOneWidget,
          reason: variant,
        );
      }
    });

    for (final String variant in <String>['plain', 'two-step', 'controlled']) {
      testWidgets('the $variant button opens its page and shows who signed in', (
        WidgetTester tester,
      ) async {
        fakeBrowser(
          (Uri url) => Uri(
            scheme: webLoginCallbackScheme,
            host: 'callback',
            queryParameters: <String, String>{
              'email': 'qa.pilot@tapwing.dev',
              'state': url.queryParameters['state']!,
            },
          ).toString(),
        );
        await openBrowserLogin(tester);
        await tester.tap(find.text('Sign in ($variant)'));
        await tester.pumpAndSettle();

        expect(find.text('Signed in as qa.pilot@tapwing.dev'), findsOneWidget);
        final Uri url = opened.single;
        expect('${url.scheme}://${url.authority}', webLoginBaseUrl);
        expect(url.path, '/$variant/login');
        expect(
          url.queryParameters['redirect_uri'],
          '$webLoginCallbackScheme://callback',
        );
        expect(url.queryParameters['state'], isNotEmpty);
      });
    }

    testWidgets('a callback with another state is not a sign-in', (
      WidgetTester tester,
    ) async {
      fakeBrowser(
        (Uri url) =>
            '$webLoginCallbackScheme://callback?email=qa.pilot%40tapwing.dev&state=stale',
      );
      await openBrowserLogin(tester);
      await tester.tap(find.text('Sign in (plain)'));
      await tester.pumpAndSettle();

      expect(find.text('Sign-in failed: state mismatch'), findsOneWidget);
      expect(find.textContaining('Signed in as'), findsNothing);
    });

    testWidgets('closing the browser reports a cancelled sign-in', (
      WidgetTester tester,
    ) async {
      fakeBrowser(
        (Uri url) => throw PlatformException(
          code: 'CANCELED',
          message: 'User canceled login',
        ),
      );
      await openBrowserLogin(tester);
      await tester.tap(find.text('Sign in (two-step)'));
      await tester.pumpAndSettle();

      expect(find.text('Sign-in cancelled'), findsOneWidget);
    });

    testWidgets('the buttons are disabled while the browser is open', (
      WidgetTester tester,
    ) async {
      final Completer<String> browser = Completer<String>();
      fakeBrowser((Uri url) => browser.future);
      await openBrowserLogin(tester);
      await tester.tap(find.text('Sign in (controlled)'));
      await tester.pump();

      expect(find.text('Signing in'), findsOneWidget);
      for (final FilledButton button in tester.widgetList<FilledButton>(
        find.byType(FilledButton),
      )) {
        expect(button.onPressed, isNull);
      }

      browser.complete(
        '$webLoginCallbackScheme://callback?email=a%40b.dev&state=${opened.single.queryParameters['state']}',
      );
      await tester.pumpAndSettle();
      expect(find.text('Signed in as a@b.dev'), findsOneWidget);
      expect(
        tester
            .widget<FilledButton>(
              find.widgetWithText(FilledButton, 'Sign in (plain)'),
            )
            .onPressed,
        isNotNull,
      );
    });
  });
}
