// F7 spike: does the layout overflow detector work in PROFILE builds, where
// Flutter compiles its own overflow report out? Not part of the sample app.
//
//   flutter build apk --profile --target-platform android-arm64 \
//     -t lib/main_overflow_spike.dart [--dart-define=SPIKE=app]
//
// SPIKE=gallery (default): known overflow / non-overflow cases, checked
//   against expectations and against Flutter's own `OVERFLOWING` flag
//   (`RenderFlex.toStringShort`, kept in profile), then scan timings.
// SPIKE=app: the real sample app with TapwingBinding, scanned every second
//   while a Tapwing flow drives it (scan cost on real screens).
//
// Results are printed as `SPIKE ...` lines (read with `adb logcat -s flutter`).
// ignore_for_file: depend_on_referenced_packages, implementation_imports
import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:tapwing_driver/tapwing_driver.dart';
import 'package:tapwing_driver/src/diagnostics/overflow_detector.dart';

import 'main.dart' as app;

const String _mode = String.fromEnvironment('SPIKE', defaultValue: 'gallery');

void main() {
  final String build = kReleaseMode
      ? 'release'
      : (kProfileMode ? 'profile' : 'debug');
  debugPrint('SPIKE mode=$_mode build=$build');
  if (_mode == 'app') {
    TapwingBinding.ensureInitialized();
    app.main();
    Timer.periodic(const Duration(seconds: 1), (_) => _scanApp());
    return;
  }
  runApp(const _SpikeApp());
}

void _scanApp() {
  final ScanStats stats = ScanStats();
  final Stopwatch sw = Stopwatch()..start();
  final List<LayoutOverflow> found = OverflowDetector.scan(stats: stats);
  sw.stop();
  debugPrint(
    'SPIKE app visited=${stats.visited} us=${sw.elapsedMicroseconds} '
    'found=${found.map((LayoutOverflow o) => o.message).toList()}',
  );
}

const Widget _long = Text('A very long product name that never fits');

/// Case id -> (widget, expected to be reported).
final Map<String, (Widget, bool)> _cases = <String, (Widget, bool)>{
  'row_text': (const Row(children: <Widget>[Icon(Icons.star), _long]), true),
  'column': (
    const Column(
      children: <Widget>[
        SizedBox(width: 20, height: 30),
        SizedBox(width: 20, height: 30),
      ],
    ),
    true,
  ),
  'spacing': (
    const Row(
      spacing: 30,
      children: <Widget>[
        SizedBox(width: 60, height: 10),
        SizedBox(width: 60, height: 10),
        SizedBox(width: 60, height: 10),
      ],
    ),
    true,
  ),
  'expanded_fixed': (
    const Row(
      children: <Widget>[
        Expanded(child: SizedBox()),
        SizedBox(width: 250, height: 10),
      ],
    ),
    true,
  ),
  'subpixel': (
    const Row(children: <Widget>[SizedBox(width: 200.5, height: 10)]),
    true,
  ),
  'rtl': (
    const Directionality(
      textDirection: TextDirection.rtl,
      child: Row(children: <Widget>[_long]),
    ),
    true,
  ),
  'unconstrained': (
    const UnconstrainedBox(child: SizedBox(width: 260, height: 10)),
    true,
  ),
  'exclude_semantics': (
    const ExcludeSemantics(child: Row(children: <Widget>[_long])),
    true,
  ),
  'offstage': (const Offstage(child: Row(children: <Widget>[_long])), false),
  'opacity_zero': (
    const Opacity(opacity: 0, child: Row(children: <Widget>[_long])),
    false,
  ),
  'indexed_hidden': (
    const IndexedStack(
      children: <Widget>[
        SizedBox(),
        Row(children: <Widget>[_long]),
      ],
    ),
    false,
  ),
  'unconstrained_clip': (
    const UnconstrainedBox(
      clipBehavior: Clip.hardEdge,
      child: SizedBox(width: 260, height: 10),
    ),
    false,
  ),
  'ellipsis': (
    const Row(
      children: <Widget>[
        Expanded(
          child: Text('Long text, ellipsized', overflow: TextOverflow.ellipsis),
        ),
      ],
    ),
    false,
  ),
  'hscroll': (
    const SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      child: Row(children: <Widget>[_long]),
    ),
    false,
  ),
  'list': (
    ListView(
      itemExtent: 36,
      children: const <Widget>[
        _Case(
          id: 'list_visible',
          child: Row(children: <Widget>[_long]),
        ),
        // Laid out in the cache extent, never painted.
        _Case(
          id: 'list_cache',
          child: Row(children: <Widget>[_long]),
        ),
      ],
    ),
    false,
  ),
};

const Set<String> _expected = <String>{
  'row_text',
  'column',
  'spacing',
  'expanded_fixed',
  'subpixel',
  'rtl',
  'unconstrained',
  'exclude_semantics',
  'list_visible',
};

class _SpikeApp extends StatelessWidget {
  const _SpikeApp();

  @override
  Widget build(BuildContext context) => MaterialApp(
    debugShowCheckedModeBanner: false,
    // The home route overflows too, but gets covered by the gallery route.
    home: const _CoveredHome(),
  );
}

class _CoveredHome extends StatefulWidget {
  const _CoveredHome();

  @override
  State<_CoveredHome> createState() => _CoveredHomeState();
}

class _CoveredHomeState extends State<_CoveredHome> {
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addPostFrameCallback((_) async {
      unawaited(
        Navigator.of(context)
            .push(MaterialPageRoute<void>(builder: (_) => const _Gallery())),
      );
      await Future<void>.delayed(const Duration(seconds: 2));
      _verify();
      await _bench('gallery');
      if (!mounted) {
        return;
      }
      unawaited(
        Navigator.of(context)
            .push(MaterialPageRoute<void>(builder: (_) => const _Stress())),
      );
      await Future<void>.delayed(const Duration(seconds: 2));
      await _bench('stress');
      debugPrint('SPIKE done');
    });
  }

  @override
  Widget build(BuildContext context) => const Scaffold(
    body: SafeArea(
      child: _Case(
        id: 'covered_route',
        child: SizedBox(width: 150, child: Row(children: <Widget>[_long])),
      ),
    ),
  );
}

class _Gallery extends StatelessWidget {
  const _Gallery();

  @override
  Widget build(BuildContext context) => Scaffold(
    body: SafeArea(
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: <Widget>[
          for (final MapEntry<String, (Widget, bool)> c in _cases.entries)
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 2),
              child: Row(
                children: <Widget>[
                  SizedBox(
                    width: 130,
                    child: Text(c.key, style: const TextStyle(fontSize: 12)),
                  ),
                  _Case(
                    id: c.key,
                    child: SizedBox(width: 200, height: 36, child: c.value.$1),
                  ),
                ],
              ),
            ),
        ],
      ),
    ),
  );
}

/// A realistic busy screen: a list of material cards with nested rows.
class _Stress extends StatelessWidget {
  const _Stress();

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('Stress')),
    body: ListView.builder(
      itemCount: 200,
      itemBuilder: (BuildContext context, int i) => Card(
        child: ListTile(
          leading: CircleAvatar(child: Text('$i')),
          title: Text('Product $i'),
          subtitle: Row(
            children: <Widget>[
              const Icon(Icons.star, size: 14),
              Text(' ${(i % 5) + 1}.0'),
              const Spacer(),
              Chip(label: Text('${i * 3} CZK')),
            ],
          ),
          trailing: IconButton(
            onPressed: () {},
            icon: const Icon(Icons.favorite_border),
          ),
        ),
      ),
    ),
  );
}

void _verify() {
  // 1. Visible overflows, attributed to their case.
  final Set<String> found = <String>{};
  for (final LayoutOverflow o in OverflowDetector.scan()) {
    final String id = _caseOf(o.renderObject) ?? '?';
    found.add(id);
    debugPrint(
      'SPIKE found $id: ${o.message} bounds=${OverflowDetector.formatRect(o.globalBounds)} text="${o.text}"',
    );
  }
  final Set<String> missing = _expected.difference(found);
  final Set<String> extra = found.difference(_expected);
  debugPrint(
    'SPIKE verify ${missing.isEmpty && extra.isEmpty ? 'PASS' : 'FAIL'} '
    'found=${found.length}/${_expected.length} missing=$missing extra=$extra',
  );

  // 2. Oracle: Flutter's own overflow flag (computed during layout in every
  // build mode) against the detector's geometry, for every RenderFlex in the
  // whole tree, painted or not.
  int agree = 0;
  int flutterOverflowing = 0;
  final List<String> disagree = <String>[];
  void visit(RenderObject node) {
    // Zero-size flexes are never painted, so neither side reports them.
    if (node is RenderFlex && node.hasSize && !node.size.isEmpty) {
      final bool flutter = node.toStringShort().endsWith(' OVERFLOWING');
      final bool detector = OverflowDetector.overflowOf(node) != null;
      if (flutter) {
        flutterOverflowing++;
      }
      if (flutter == detector) {
        agree++;
      } else {
        disagree.add('${_caseOf(node)} flutter=$flutter detector=$detector');
      }
    }
    node.visitChildren(visit);
  }

  for (final RenderView view in RendererBinding.instance.renderViews) {
    visit(view);
  }
  debugPrint(
    'SPIKE oracle ${disagree.isEmpty ? 'PASS' : 'FAIL'} flexes=${agree + disagree.length} '
    'flutterOverflowing=$flutterOverflowing disagree=$disagree',
  );
}

Future<void> _bench(String label) async {
  const int runs = 300;
  final List<int> us = <int>[];
  final ScanStats stats = ScanStats();
  for (int i = 0; i < runs; i++) {
    final Stopwatch sw = Stopwatch()..start();
    OverflowDetector.scan(stats: stats);
    sw.stop();
    us.add(sw.elapsedMicroseconds);
    if (i % 20 == 0) {
      // Yield so frames and the device keep going, like scans between steps.
      await Future<void>.delayed(Duration.zero);
    }
  }
  us.sort();
  debugPrint(
    'SPIKE bench $label visited=${stats.visited} runs=$runs '
    'p50=${us[runs ~/ 2]}us p95=${us[(runs * 95) ~/ 100]}us max=${us.last}us',
  );
}

String? _caseOf(RenderObject node) {
  for (RenderObject? n = node; n != null; n = n.parent) {
    if (n is _RenderCase) {
      return n.id;
    }
  }
  return null;
}

class _Case extends SingleChildRenderObjectWidget {
  const _Case({required this.id, super.child});

  final String id;

  @override
  RenderObject createRenderObject(BuildContext context) => _RenderCase(id);
}

class _RenderCase extends RenderProxyBox {
  _RenderCase(this.id);

  final String id;
}
