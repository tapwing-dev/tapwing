// Static guard: every element id and every screen title the Android and iOS
// Maestro flows rely on must still exist in the sample app's source. Renaming
// an identifier in `lib/main.dart` without updating `.maestro/` (or the other
// way around) fails here instead of on a device.
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';

/// Flow files under `.maestro/` (Android flows, iOS variants, negative flows).
List<File> _flowFiles() {
  final Directory dir = Directory('.maestro');
  expect(
    dir.existsSync(),
    isTrue,
    reason: 'run `flutter test` from examples/sample_app',
  );
  return dir
      .listSync(recursive: true)
      .whereType<File>()
      .where((File f) => f.path.endsWith('.yaml') || f.path.endsWith('.yml'))
      .toList()
    ..sort((File a, File b) => a.path.compareTo(b.path));
}

/// `id: "name"` selectors in a flow, ignoring `appId:`/`resource-id`.
Set<String> _idsUsedBy(File flow) {
  final RegExp re = RegExp(
    r'''^\s*(?:-\s*)?id:\s*["']?([A-Za-z0-9_.\-]+)["']?\s*$''',
  );
  return <String>{
    for (final String line in flow.readAsLinesSync())
      if (re.firstMatch(line) case final RegExpMatch m) m.group(1)!,
  };
}

void main() {
  final String source = File('lib/main.dart').readAsStringSync();
  final Set<String> declaredIds = <String>{
    for (final RegExpMatch m in RegExp(
      r'''identifier:\s*['"]([A-Za-z0-9_.\-]+)['"]''',
    ).allMatches(source))
      m.group(1)!,
  };

  test('the sample app declares Semantics identifiers', () {
    expect(declaredIds, containsAll(<String>['email_input', 'login_button']));
    expect(declaredIds.length, greaterThan(20));
  });

  test('the Maestro flows exist for both platforms', () {
    final List<String> paths = _flowFiles().map((File f) => f.path).toList();
    expect(paths, isNotEmpty);
    expect(paths.any((String p) => p.contains('login_and_browse')), isTrue);
    expect(
      paths.any(
        (String p) =>
            p.contains('${Platform.pathSeparator}ios${Platform.pathSeparator}'),
      ),
      isTrue,
    );
    expect(paths.any((String p) => p.contains('negative')), isTrue);
  });

  test('every `id:` selector used by a flow is a Semantics identifier in lib/main.dart', () {
    final Map<String, Set<String>> missing = <String, Set<String>>{};
    for (final File flow in _flowFiles()) {
      final Set<String> unknown = _idsUsedBy(flow).difference(declaredIds);
      if (unknown.isNotEmpty) missing[flow.path] = unknown;
    }
    expect(
      missing,
      isEmpty,
      reason: 'flows select ids the app does not declare',
    );
  });

  test('the id extraction itself finds ids (guards the guard)', () {
    final Set<String> all = <String>{
      for (final File f in _flowFiles()) ..._idsUsedBy(f),
    };
    expect(
      all,
      containsAll(<String>[
        'login_button',
        'password_input',
        'favorite_switch',
      ]),
    );
  });

  test('every flow has a config section with the sample app id', () {
    // Android package `dev.tapwing.sample_app`, iOS bundle id `dev.tapwing.sampleApp`.
    final RegExp appId = RegExp(
      r'^appId: dev\.tapwing\.(sample_app|sampleApp)$',
      multiLine: true,
    );
    for (final File flow in _flowFiles()) {
      final String text = flow.readAsStringSync();
      expect(appId.hasMatch(text), isTrue, reason: flow.path);
      expect(text, contains('\n---\n'), reason: flow.path);
    }
  });

  test(
    'the on-screen strings asserted by login_and_browse.yaml exist in the app',
    () {
      for (final String shown in <String>[
        'Welcome back',
        'Email is required',
        'Products',
        'Details of Product',
        'New product',
        'Added ',
        'Favorite',
      ]) {
        expect(
          source,
          contains(shown),
          reason: 'login_and_browse.yaml asserts "$shown"',
        );
      }
    },
  );

  group('browser login flows', () {
    // The login pages and their one account live in bench/web-login.
    final File server = File('../../bench/web-login/server.py');
    List<File> webFlows() => _flowFiles()
        .where((File f) => f.uri.pathSegments.contains('web'))
        .toList();

    test('exist for every page variant on both platforms', () {
      final List<String> paths = webFlows()
          .map((File f) => f.uri.pathSegments.join('/'))
          .toList();
      for (final String variant in <String>['plain', 'two_step', 'controlled']) {
        expect(paths, contains('.maestro/web/web_login_$variant.yaml'));
        expect(paths, contains('.maestro/ios/web/web_login_$variant.yaml'));
        expect(source, contains("identifier: 'browser_login_$variant'"));
      }
    });

    test('are not top-level, so a workspace run without the server skips them', () {
      final Iterable<File> topLevel = Directory('.maestro')
          .listSync()
          .whereType<File>()
          .where((File f) => f.path.endsWith('.yaml'));
      for (final File flow in topLevel) {
        expect(
          flow.readAsStringSync(),
          isNot(contains('browser_login_')),
          reason: flow.path,
        );
      }
    });

    test('type the account the login server accepts and assert the app\'s result', () {
      expect(server.existsSync(), isTrue, reason: server.path);
      final String py = server.readAsStringSync();
      final String email = RegExp(r'^EMAIL = "(.+)"$', multiLine: true)
          .firstMatch(py)!
          .group(1)!;
      final String password = RegExp(r'^PASSWORD = "(.+)"$', multiLine: true)
          .firstMatch(py)!
          .group(1)!;
      expect(source, contains("'Signed in as \$email'"));
      for (final File flow in webFlows()) {
        final String text = flow.readAsStringSync();
        expect(text, contains('- inputText: "$email"'), reason: flow.path);
        expect(text, contains('- inputText: "$password"'), reason: flow.path);
        expect(
          text,
          contains('- assertVisible: "Signed in as $email"'),
          reason: flow.path,
        );
      }
    });

    test('use the callback scheme the Android manifest declares', () {
      final String scheme = RegExp(
        r"const String webLoginCallbackScheme = '([a-z][a-z0-9+.-]*)';",
      ).firstMatch(source)!.group(1)!;
      expect(
        File('android/app/src/main/AndroidManifest.xml').readAsStringSync(),
        contains('<data android:scheme="$scheme"/>'),
      );
      expect(
        server.readAsStringSync(),
        contains('redirect_scheme = "$scheme"'),
      );
    });
  });

  test(
    'the instrumented entrypoint wires the driver around the unchanged app',
    () {
      final String entry = File('lib/main_tapwing.dart').readAsStringSync();
      expect(entry, contains('TapwingBinding.ensureInitialized();'));
      expect(entry, contains("import 'main.dart' as app;"));
      expect(entry, contains('app.main();'));
      // Production code never depends on the driver.
      expect(source, isNot(contains('tapwing_driver')));
    },
  );
}
