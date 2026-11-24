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
