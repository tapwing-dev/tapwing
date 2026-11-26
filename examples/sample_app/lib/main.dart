import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';
import 'package:shared_preferences/shared_preferences.dart';

void main() => runApp(const SampleApp());

class SampleApp extends StatelessWidget {
  const SampleApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Tapwing Sample',
      theme: ThemeData(colorSchemeSeed: Colors.teal),
      darkTheme: ThemeData(
        colorSchemeSeed: Colors.teal,
        brightness: Brightness.dark,
      ),
      themeMode: ThemeMode.system,
      // iOS only: unfocus text fields on taps outside them (standard iOS UX,
      // like Flutter on desktop and web). Flutter on iOS keeps focus, which
      // makes stock Maestro's iOS `hideKeyboard` fail (see
      // spikes/ios/hide-keyboard/REPORT.md). Scoped to iOS so the Android F2
      // flows that exercise `back` with an open keyboard are unaffected.
      builder: defaultTargetPlatform == TargetPlatform.iOS && _iosTapOutsideUnfocus
          ? _unfocusOnTapOutside
          : null,
      home: const LoginScreen(),
    );
  }
}

/// `--dart-define=SAMPLE_IOS_TAP_OUTSIDE_UNFOCUS=false` builds the stock Flutter
/// behavior, used to reproduce Maestro's iOS `hideKeyboard` failure.
const bool _iosTapOutsideUnfocus = bool.fromEnvironment(
  'SAMPLE_IOS_TAP_OUTSIDE_UNFOCUS',
  defaultValue: true,
);

Widget _unfocusOnTapOutside(BuildContext context, Widget? child) => Actions(
      actions: <Type, Action<Intent>>{
        EditableTextTapOutsideIntent: CallbackAction<EditableTextTapOutsideIntent>(
          onInvoke: (EditableTextTapOutsideIntent intent) {
            intent.focusNode.unfocus();
            return null;
          },
        ),
      },
      child: child!,
    );

class LoginScreen extends StatefulWidget {
  const LoginScreen({super.key});

  @override
  State<LoginScreen> createState() => _LoginScreenState();
}

class _LoginScreenState extends State<LoginScreen> {
  final TextEditingController _email = TextEditingController();
  final TextEditingController _password = TextEditingController();
  bool _loading = false;
  String? _error;

  Future<void> _login() async {
    if (_email.text.isEmpty) {
      setState(() => _error = 'Email is required');
      return;
    }
    setState(() {
      _error = null;
      _loading = true;
    });
    // Simulated backend call.
    await Future<void>.delayed(const Duration(milliseconds: 600));
    if (!mounted) return;
    setState(() => _loading = false);
    await Navigator.of(context).pushReplacement(
      MaterialPageRoute<void>(builder: (_) => const ProductsScreen()),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: SafeArea(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: <Widget>[
              const SizedBox(height: 48),
              Text('Welcome back', style: Theme.of(context).textTheme.headlineMedium),
              const SizedBox(height: 24),
              Semantics(
                identifier: 'email_input',
                child: TextField(
                  controller: _email,
                  keyboardType: TextInputType.emailAddress,
                  decoration: const InputDecoration(labelText: 'Email'),
                ),
              ),
              const SizedBox(height: 12),
              Semantics(
                identifier: 'password_input',
                child: TextField(
                  controller: _password,
                  obscureText: true,
                  decoration: const InputDecoration(labelText: 'Password'),
                ),
              ),
              if (_error != null) ...<Widget>[
                const SizedBox(height: 12),
                Text(_error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
              ],
              const SizedBox(height: 24),
              Semantics(
                identifier: 'login_button',
                child: FilledButton(
                  onPressed: _loading ? null : _login,
                  child: _loading
                      ? const SizedBox.square(
                          dimension: 20,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Text('Log in'),
                ),
              ),
              const SizedBox(height: 12),
              Semantics(
                identifier: 'playground_button',
                child: TextButton(
                  onPressed: () => Navigator.of(context).push(
                    MaterialPageRoute<void>(builder: (_) => const PlaygroundScreen()),
                  ),
                  child: const Text('F2 Playground'),
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class ProductsScreen extends StatelessWidget {
  const ProductsScreen({super.key});

  Future<void> _add(BuildContext context) async {
    final TextEditingController name = TextEditingController();
    final String? result = await showDialog<String>(
      context: context,
      builder: (BuildContext ctx) => AlertDialog(
        title: const Text('New product'),
        content: TextField(
          controller: name,
          autofocus: false,
          decoration: const InputDecoration(labelText: 'Name'),
        ),
        actions: <Widget>[
          TextButton(onPressed: () => Navigator.pop(ctx), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(ctx, name.text), child: const Text('Add')),
        ],
      ),
    );
    if (result != null && result.isNotEmpty && context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('Added $result')));
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Products')),
      floatingActionButton: FloatingActionButton(
        tooltip: 'Add product',
        onPressed: () => _add(context),
        child: const Icon(Icons.add),
      ),
      body: ListView.builder(
        itemCount: 100,
        itemBuilder: (BuildContext context, int i) => ListTile(
          title: Text('Product ${i + 1}'),
          subtitle: Text('€ ${(i + 1) * 3}.00'),
          onTap: () => Navigator.of(context).push(
            MaterialPageRoute<void>(builder: (_) => DetailScreen(index: i + 1)),
          ),
        ),
      ),
    );
  }
}

class DetailScreen extends StatefulWidget {
  const DetailScreen({super.key, required this.index});
  final int index;

  @override
  State<DetailScreen> createState() => _DetailScreenState();
}

class _DetailScreenState extends State<DetailScreen> {
  bool _favorite = false;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text('Product ${widget.index}')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: <Widget>[
          Text('Details of Product ${widget.index}'),
          const SizedBox(height: 16),
          Semantics(
            identifier: 'favorite_switch',
            child: SwitchListTile(
              title: const Text('Favorite'),
              value: _favorite,
              onChanged: (bool v) => setState(() => _favorite = v),
            ),
          ),
        ],
      ),
    );
  }
}

/// Hub screen for differential testing of all F2 Maestro commands.
class PlaygroundScreen extends StatefulWidget {
  const PlaygroundScreen({super.key});

  @override
  State<PlaygroundScreen> createState() => _PlaygroundScreenState();
}

class _PlaygroundScreenState extends State<PlaygroundScreen> {
  /// Android-only runtime permission channel (see `MainActivity.kt`).
  static const MethodChannel _permissions = MethodChannel('dev.tapwing.sample_app/permissions');
  /// Async plugin-style channel used to verify settle waits on pending replies.
  static const MethodChannel _asyncChannel = MethodChannel('dev.tapwing.sample_app/async');

  String _permissionStatus = 'Permission: unknown';

  /// Asks for `ACCESS_FINE_LOCATION`: the system shows its own dialog, which
  /// only the native lane can drive.
  Future<void> _requestLocationPermission() async {
    String status;
    try {
      final bool granted = await _permissions.invokeMethod<bool>('requestLocation') ?? false;
      status = granted ? 'Permission: granted' : 'Permission: denied';
    } on MissingPluginException {
      status = 'Permission: unsupported';
    } on PlatformException {
      status = 'Permission: denied';
    }
    if (mounted) {
      setState(() => _permissionStatus = status);
    }
  }

  /// Awaits an async method-channel reply before navigating, without showing a
  /// spinner or scheduling a frame while the platform call is in flight.
  Future<void> _awaitChannelAndNavigate() async {
    String result = 'completed';
    try {
      result = await _asyncChannel.invokeMethod<String>('fetchAndNavigate') ?? 'completed';
    } on MissingPluginException {
      result = 'unsupported';
    }
    if (!mounted) return;
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => Scaffold(
          appBar: AppBar(title: const Text('Async Channel Done')),
          body: Center(child: Text('Channel result: $result')),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('F2 Playground')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: <Widget>[
          Semantics(
            identifier: 'nav_selectors_taps',
            child: ListTile(
              title: const Text('Selectors & Taps'),
              subtitle: const Text('traits, css, point, doubleTap, longPress, retry'),
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(builder: (_) => const SelectorsAndTapsScreen()),
              ),
            ),
          ),
          Semantics(
            identifier: 'nav_swipe_scroll',
            child: ListTile(
              title: const Text('Swipe & Scroll'),
              subtitle: const Text('direction, coords, from element, scrollUntilVisible'),
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(builder: (_) => const SwipeAndScrollScreen()),
              ),
            ),
          ),
          Semantics(
            identifier: 'nav_text_keys',
            child: ListTile(
              title: const Text('Text & Keys'),
              subtitle: const Text('copy/paste/clipboard, inputRandom*, pressKey'),
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(builder: (_) => const TextAndKeysScreen()),
              ),
            ),
          ),
          Semantics(
            identifier: 'nav_screenshot_theme',
            child: ListTile(
              title: const Text('Screenshot & Theme'),
              subtitle: const Text('takeScreenshot, assertScreenshot, dark/light mode'),
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(builder: (_) => const ScreenshotAndThemeScreen()),
              ),
            ),
          ),
          Semantics(
            identifier: 'nav_endless_animation',
            child: ListTile(
              title: const Text('Endless Animation'),
              subtitle: const Text('large repeating animation (> 2% screen) settle test'),
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(builder: (_) => const EndlessAnimationScreen()),
              ),
            ),
          ),
          Semantics(
            identifier: 'permission_status',
            child: ListTile(
              title: Text(_permissionStatus),
              subtitle: const Text('system dialog, driven by the native lane'),
            ),
          ),
          Semantics(
            identifier: 'request_location_permission',
            child: ListTile(
              title: const Text('Request Location Permission'),
              onTap: _requestLocationPermission,
            ),
          ),
          Semantics(
            identifier: 'async_channel_nav',
            child: ListTile(
              title: const Text('Async Channel Nav'),
              subtitle: const Text('awaits method channel reply then navigates'),
              onTap: _awaitChannelAndNavigate,
            ),
          ),
          Semantics(
            identifier: 'nav_network',
            child: ListTile(
              title: const Text('Network'),
              subtitle: const Text('F8b: HTTP requests to an in-app loopback server'),
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(builder: (_) => const NetworkScreen()),
              ),
            ),
          ),
          Semantics(
            identifier: 'nav_app_health',
            child: ListTile(
              title: const Text('App Health'),
              subtitle: const Text('F7: overflow, broken image, build/async errors, crash'),
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(builder: (_) => const AppHealthScreen()),
              ),
            ),
          ),
          Semantics(
            identifier: 'nav_app_state',
            child: ListTile(
              title: const Text('App State'),
              subtitle: const Text('a preference and a file that clearState must remove'),
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(builder: (_) => const AppStateScreen()),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class SelectorsAndTapsScreen extends StatefulWidget {
  const SelectorsAndTapsScreen({super.key});

  @override
  State<SelectorsAndTapsScreen> createState() => _SelectorsAndTapsScreenState();
}

class _SelectorsAndTapsScreenState extends State<SelectorsAndTapsScreen> {
  final TextEditingController _longText = TextEditingController(
    text:
        'Tapwing long text sample exceeding two hundred characters so that Android AccessibilityNodeInfo text length passes the Maestro LONG_TEXT trait threshold of strictly greater than two hundred characters without any ambiguity whatsoever.',
  );
  String _splitHit = 'Split: none';
  bool _doubleTapped = false;
  bool _longPressed = false;
  int _retryClicks = 0;
  bool _retrySucceeded = false;
  bool _showCoveredSection = false;
  bool _showCoverOverlay = false;
  bool _coveredTapped = false;
  bool _slowTaskRunning = false;
  Timer? _overlayTimer;
  Timer? _slowTaskTimer;

  @override
  void dispose() {
    _overlayTimer?.cancel();
    _slowTaskTimer?.cancel();
    _longText.dispose();
    super.dispose();
  }

  void _triggerCoveredTarget() {
    _overlayTimer?.cancel();
    setState(() {
      _showCoveredSection = true;
      _showCoverOverlay = true;
      _coveredTapped = false;
    });
    _overlayTimer = Timer(const Duration(milliseconds: 900), () {
      if (mounted) {
        setState(() => _showCoverOverlay = false);
      }
    });
  }

  void _startSlowTask() {
    _slowTaskTimer?.cancel();
    setState(() => _slowTaskRunning = true);
    // Disappears after 8.5 s (> old 7 s bug, < Maestro 17 s assertNotVisible timeout).
    _slowTaskTimer = Timer(const Duration(milliseconds: 8500), () {
      if (mounted) {
        setState(() => _slowTaskRunning = false);
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Selectors & Taps')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: <Widget>[
          const Text('Traits Ready'),
          const SizedBox(height: 12),
          Row(
            children: <Widget>[
              Semantics(
                identifier: 'square_box',
                label: 'Square box',
                container: true,
                excludeSemantics: true,
                child: const SizedBox(
                  width: 90,
                  height: 90,
                  child: ColoredBox(color: Colors.teal),
                ),
              ),
              const SizedBox(width: 16),
              Expanded(
                child: Semantics(
                  identifier: 'long_text_field',
                  child: TextField(
                    controller: _longText,
                    readOnly: true,
                    maxLines: 2,
                    decoration: const InputDecoration(
                      labelText: 'Long text',
                      border: OutlineInputBorder(),
                    ),
                  ),
                ),
              ),
            ],
          ),
          const SizedBox(height: 16),
          Text(_splitHit),
          const SizedBox(height: 8),
          Semantics(
            identifier: 'split_target',
            label: 'Split target',
            container: true,
            excludeSemantics: true,
            child: SizedBox(
              height: 56,
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: <Widget>[
                  Expanded(
                    child: GestureDetector(
                      onTap: () => setState(() => _splitHit = 'Left hit'),
                      child: ColoredBox(
                        color: Colors.blue.shade200,
                        child: const Center(child: Text('Left')),
                      ),
                    ),
                  ),
                  Expanded(
                    child: GestureDetector(
                      onTap: () => setState(() => _splitHit = 'Right hit'),
                      child: ColoredBox(
                        color: Colors.orange.shade200,
                        child: const Center(child: Text('Right')),
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
          const SizedBox(height: 16),
          GestureDetector(
            onDoubleTap: () => setState(() => _doubleTapped = true),
            child: Container(
              height: 48,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                border: Border.all(color: Colors.teal),
                borderRadius: BorderRadius.circular(8),
              ),
              child: Text(_doubleTapped ? 'Double tapped' : 'Double tap me'),
            ),
          ),
          const SizedBox(height: 12),
          GestureDetector(
            onLongPress: () => setState(() => _longPressed = true),
            child: Container(
              height: 48,
              alignment: Alignment.center,
              decoration: BoxDecoration(
                border: Border.all(color: Colors.teal),
                borderRadius: BorderRadius.circular(8),
              ),
              child: Text(_longPressed ? 'Long pressed' : 'Long press me'),
            ),
          ),
          const SizedBox(height: 12),
          Semantics(
            identifier: 'retry_button',
            child: FilledButton(
              onPressed: () {
                _retryClicks++;
                // First tap produces no hierarchy change so retryTapIfNoChange
                // retries automatically and triggers the second tap.
                if (_retryClicks >= 2) {
                  setState(() => _retrySucceeded = true);
                }
              },
              child: Text(_retrySucceeded ? 'Retry succeeded' : 'Tap twice for change'),
            ),
          ),
          const SizedBox(height: 12),
          FilledButton(
            onPressed: _triggerCoveredTarget,
            child: const Text('Show covered target'),
          ),
          if (_showCoveredSection) ...<Widget>[
            const SizedBox(height: 8),
            SizedBox(
              height: 52,
              child: Stack(
                fit: StackFit.expand,
                children: <Widget>[
                  FilledButton(
                    onPressed: () => setState(() => _coveredTapped = true),
                    child: Text(_coveredTapped ? 'Covered tapped' : 'Covered target'),
                  ),
                  if (_showCoverOverlay)
                    Semantics(
                      container: true,
                      label: 'Temporary cover',
                      child: GestureDetector(
                        behavior: HitTestBehavior.opaque,
                        onTap: () {},
                        child: const ColoredBox(
                          color: Colors.amber,
                          child: Center(child: Text('Temporary cover')),
                        ),
                      ),
                    ),
                ],
              ),
            ),
          ],
          const SizedBox(height: 12),
          FilledButton(
            onPressed: _startSlowTask,
            child: const Text('Start 8.5s task'),
          ),
          if (_slowTaskRunning) ...<Widget>[
            const SizedBox(height: 8),
            const Text('Working 8.5s...'),
          ],
        ],
      ),
    );
  }
}

class SwipeAndScrollScreen extends StatefulWidget {
  const SwipeAndScrollScreen({super.key});

  @override
  State<SwipeAndScrollScreen> createState() => _SwipeAndScrollScreenState();
}

enum _ScrollDemoMode { vertical, horizontal, pageView }

class _SwipeAndScrollScreenState extends State<SwipeAndScrollScreen> {
  bool _dismissedA = false;
  bool _dismissedB = false;
  _ScrollDemoMode _mode = _ScrollDemoMode.vertical;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Swipe & Scroll'),
        actions: <Widget>[
          TextButton(
            onPressed: () => setState(() {
              _mode = _mode == _ScrollDemoMode.horizontal
                  ? _ScrollDemoMode.vertical
                  : _ScrollDemoMode.horizontal;
            }),
            child: Text(_mode == _ScrollDemoMode.horizontal ? 'Vertical mode' : 'Horizontal mode'),
          ),
          TextButton(
            onPressed: () => setState(() => _mode = _ScrollDemoMode.pageView),
            child: const Text('PageView mode'),
          ),
        ],
      ),
      body: switch (_mode) {
        _ScrollDemoMode.pageView => PageView(
            children: const <Widget>[
              ColoredBox(
                color: Color(0xFFE0F2F1),
                child: Center(child: Text('Snap Page 1')),
              ),
              ColoredBox(
                color: Color(0xFFB2DFDB),
                child: Center(child: Text('Snap Page 2')),
              ),
              ColoredBox(
                color: Color(0xFF80CBC4),
                child: Center(child: Text('Snap Page 3')),
              ),
              ColoredBox(
                color: Color(0xFF4DB6AC),
                child: Center(child: Text('Snap Page 4')),
              ),
            ],
          ),
        _ScrollDemoMode.horizontal => ListView.builder(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 48),
            itemCount: 25,
            itemBuilder: (BuildContext context, int i) => Card(
              child: SizedBox(
                width: 140,
                child: Center(child: Text('HCard ${i + 1}')),
              ),
            ),
          ),
        _ScrollDemoMode.vertical => Column(
              children: <Widget>[
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
                  child: _dismissedA
                      ? const SizedBox(
                          height: 52,
                          child: Center(child: Text('Dismiss A done')),
                        )
                      : Semantics(
                          identifier: 'dismiss_a',
                          child: Dismissible(
                            key: const ValueKey<String>('dismiss_a'),
                            direction: DismissDirection.endToStart,
                            onDismissed: (_) => setState(() => _dismissedA = true),
                            background: const ColoredBox(color: Colors.red),
                            child: const Card(
                              child: SizedBox(
                                height: 52,
                                child: Center(child: Text('Swipe left to dismiss A')),
                              ),
                            ),
                          ),
                        ),
                ),
                Padding(
                  padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
                  child: _dismissedB
                      ? const SizedBox(
                          height: 52,
                          child: Center(child: Text('Dismiss B done')),
                        )
                      : Semantics(
                          identifier: 'dismiss_b',
                          child: Dismissible(
                            key: const ValueKey<String>('dismiss_b'),
                            direction: DismissDirection.startToEnd,
                            onDismissed: (_) => setState(() => _dismissedB = true),
                            background: const ColoredBox(color: Colors.green),
                            child: const Card(
                              child: SizedBox(
                                height: 52,
                                child: Center(child: Text('Swipe right to dismiss B')),
                              ),
                            ),
                          ),
                        ),
                ),
                const Divider(height: 8),
                Expanded(
                  child: ListView.builder(
                    itemCount: 40,
                    itemBuilder: (BuildContext context, int i) => ListTile(
                      title: Text('VRow ${i + 1}'),
                    ),
                  ),
                ),
              ],
            ),
      },
    );
  }
}

class TextAndKeysScreen extends StatefulWidget {
  const TextAndKeysScreen({super.key});

  @override
  State<TextAndKeysScreen> createState() => _TextAndKeysScreenState();
}

class _TextAndKeysScreenState extends State<TextAndKeysScreen> {
  final TextEditingController _fieldOne = TextEditingController();
  final TextEditingController _fieldTwo = TextEditingController();
  String _submitted = '';

  @override
  void dispose() {
    _fieldOne.dispose();
    _fieldTwo.dispose();
    super.dispose();
  }

  Future<void> _openDialog() async {
    await showDialog<void>(
      context: context,
      builder: (BuildContext ctx) => AlertDialog(
        title: const Text('Key Dialog'),
        content: const Text('Dismiss with Escape or Back'),
        actions: <Widget>[
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Close'),
          ),
        ],
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Text & Keys')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: <Widget>[
          Semantics(
            identifier: 'copy_source',
            child: const Text('Tapwing-Copy-99'),
          ),
          const SizedBox(height: 12),
          Semantics(
            identifier: 'field_one',
            child: TextField(
              controller: _fieldOne,
              textInputAction: TextInputAction.done,
              onSubmitted: (String value) => setState(() => _submitted = value),
              decoration: const InputDecoration(
                labelText: 'Field One',
                border: OutlineInputBorder(),
              ),
            ),
          ),
          const SizedBox(height: 12),
          Semantics(
            identifier: 'field_two',
            child: TextField(
              controller: _fieldTwo,
              decoration: const InputDecoration(
                labelText: 'Field Two',
                border: OutlineInputBorder(),
              ),
            ),
          ),
          const SizedBox(height: 12),
          Semantics(
            identifier: 'secret_field',
            child: const TextField(
              obscureText: true,
              decoration: InputDecoration(
                labelText: 'Secret',
                border: OutlineInputBorder(),
              ),
            ),
          ),
          if (_submitted.isNotEmpty) ...<Widget>[
            const SizedBox(height: 12),
            Text('Submitted: $_submitted'),
          ],
          const SizedBox(height: 16),
          FilledButton(
            onPressed: _openDialog,
            child: const Text('Open Key Dialog'),
          ),
        ],
      ),
    );
  }
}

class ScreenshotAndThemeScreen extends StatelessWidget {
  const ScreenshotAndThemeScreen({super.key});

  @override
  Widget build(BuildContext context) {
    final bool isDark = Theme.of(context).brightness == Brightness.dark;
    return Scaffold(
      appBar: AppBar(title: const Text('Screenshot & Theme')),
      body: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            Text(isDark ? 'Theme: dark' : 'Theme: light'),
            const SizedBox(height: 24),
            Semantics(
              identifier: 'visual_badge',
              label: 'Visual badge',
              container: true,
              child: Container(
                width: 200,
                height: 88,
                decoration: BoxDecoration(
                  color: Colors.teal.shade700,
                  borderRadius: BorderRadius.circular(12),
                ),
                alignment: Alignment.center,
                child: const Text(
                  'TAPWING F2',
                  style: TextStyle(
                    color: Colors.white,
                    fontSize: 22,
                    fontWeight: FontWeight.bold,
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class EndlessAnimationScreen extends StatefulWidget {
  const EndlessAnimationScreen({super.key});

  @override
  State<EndlessAnimationScreen> createState() => _EndlessAnimationScreenState();
}

class _EndlessAnimationScreenState extends State<EndlessAnimationScreen>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 900),
  )..repeat(reverse: true);
  int _clicks = 0;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Endless Animation')),
      body: Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: <Widget>[
            FadeTransition(
              opacity: Tween<double>(begin: 0.4, end: 1.0).animate(_controller),
              child: Container(
                width: 240,
                height: 140,
                decoration: BoxDecoration(
                  color: Colors.teal.shade600,
                  borderRadius: BorderRadius.circular(16),
                ),
                alignment: Alignment.center,
                child: const Text(
                  'Pulsing Banner',
                  style: TextStyle(color: Colors.white, fontSize: 20),
                ),
              ),
            ),
            const SizedBox(height: 24),
            Text('Pulse clicks: $_clicks'),
            const SizedBox(height: 12),
            FilledButton(
              onPressed: () => setState(() => _clicks++),
              child: const Text('Pulse action'),
            ),
          ],
        ),
      ),
    );
  }
}

/// F7 app health playground: every button makes the app report a problem
/// that no assertion would notice. Flows tapping these still pass in Maestro.
class AppHealthScreen extends StatefulWidget {
  const AppHealthScreen({super.key});

  @override
  State<AppHealthScreen> createState() => _AppHealthScreenState();
}

class _AppHealthScreenState extends State<AppHealthScreen> {
  /// Android-only crash channel (see `MainActivity.kt`).
  static const MethodChannel _health = MethodChannel('dev.tapwing.sample_app/health');

  bool _overflow = false;
  bool _brokenImage = false;
  bool _buildError = false;
  bool _lateSetState = false;
  String _status = 'Health: idle';
  Timer? _floodTimer;

  @override
  void dispose() {
    _floodTimer?.cancel();
    super.dispose();
  }

  void _set(String status, VoidCallback change) => setState(() {
        change();
        _status = status;
      });

  /// Fires a sustained >= 1000 errors/s flood through uncaught async
  /// microtasks (`PlatformDispatcher.onError`). When [unique] is true, every
  /// error carries a distinct message to exercise the 500-entry ring buffer
  /// and `dropped` counter; when false, identical errors exercise
  /// deduplication (`count > 1`).
  void _startFlood({required bool unique}) {
    _floodTimer?.cancel();
    const int total = 2000;
    const int perTick = 40;
    int sent = 0;
    final Stopwatch sw = Stopwatch()..start();
    final String label = unique ? 'unique' : 'repeated';
    setState(() => _status = 'Health: flooding $label...');

    void fireRepeated() => throw StateError('Tapwing F7 flood repeated');
    void fireUnique(int index) => throw StateError('Tapwing F7 flood unique #$index');

    void emitBatch() {
      final int end = (sent + perTick) > total ? total : (sent + perTick);
      for (int i = sent; i < end; i++) {
        final int n = i;
        Future<void>.microtask(unique ? () => fireUnique(n) : fireRepeated);
      }
      sent = end;
      if (sent >= total) {
        _floodTimer?.cancel();
        _floodTimer = Timer(Duration.zero, () {
          sw.stop();
          final int ms = sw.elapsedMilliseconds == 0 ? 1 : sw.elapsedMilliseconds;
          final int rate = (total * 1000) ~/ ms;
          if (mounted) {
            setState(() => _status = 'Health: flooded $total $label in ${ms}ms ($rate/s)');
          }
        });
      }
    }

    emitBatch();
    _floodTimer = Timer.periodic(const Duration(milliseconds: 15), (_) => emitBatch());
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('App Health')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: <Widget>[
          Semantics(identifier: 'health_status', child: Text(_status)),
          const SizedBox(height: 8),
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: <Widget>[
              OutlinedButton(
                onPressed: () => _set('Health: overflow shown', () => _overflow = true),
                child: const Text('Show overflow'),
              ),
              OutlinedButton(
                onPressed: () => _set('Health: image requested', () => _brokenImage = true),
                child: const Text('Load broken image'),
              ),
              OutlinedButton(
                onPressed: () => _set('Health: build error shown', () => _buildError = true),
                child: const Text('Throw in build'),
              ),
              OutlinedButton(
                onPressed: () {
                  // Uncaught: reaches PlatformDispatcher.onError.
                  Future<void>.delayed(Duration.zero, () => throw StateError('Tapwing F7 async error'));
                  _set('Health: async error thrown', () {});
                },
                child: const Text('Throw async'),
              ),
              OutlinedButton(
                onPressed: () {
                  _set('Health: late setState scheduled', () => _lateSetState = true);
                  // Dispose it on the next frame, before its delayed setState.
                  WidgetsBinding.instance.addPostFrameCallback((_) {
                    if (mounted) setState(() => _lateSetState = false);
                  });
                },
                child: const Text('setState after dispose'),
              ),
              OutlinedButton(
                onPressed: () => _health.invokeMethod<void>('crash'),
                child: const Text('Crash (native)'),
              ),
              OutlinedButton(
                onPressed: () => exit(3),
                child: const Text('Exit process'),
              ),
              OutlinedButton(
                onPressed: () => _startFlood(unique: true),
                child: const Text('Flood unique errors'),
              ),
              OutlinedButton(
                onPressed: () => _startFlood(unique: false),
                child: const Text('Flood repeated errors'),
              ),
            ],
          ),
          const SizedBox(height: 16),
          if (_overflow)
            // Align loosens the ListView's tight width, so the row really is
            // 220 wide and overflows.
            const Align(
              alignment: Alignment.centerLeft,
              child: SizedBox(
                width: 220,
                child: Row(
                  children: <Widget>[
                    Icon(Icons.warning_amber),
                    Text('A product name far too long for this row'),
                  ],
                ),
              ),
            ),
          if (_brokenImage)
            // Nothing listens on port 9 (discard): fails fast and offline.
            Image.network('http://127.0.0.1:9/tapwing/missing.png', width: 48, height: 48),
          if (_buildError) const _ThrowsInBuild(),
          if (_lateSetState) const _SetsStateLate(),
        ],
      ),
    );
  }
}

class _ThrowsInBuild extends StatelessWidget {
  const _ThrowsInBuild();

  @override
  Widget build(BuildContext context) => throw StateError('Tapwing F7 build error');
}

/// Removed right after it is shown: its delayed setState runs after dispose
/// (a debug-mode framework assertion).
class _SetsStateLate extends StatefulWidget {
  const _SetsStateLate();

  @override
  State<_SetsStateLate> createState() => _SetsStateLateState();
}

class _SetsStateLateState extends State<_SetsStateLate> {
  @override
  void initState() {
    super.initState();
    Future<void>.delayed(const Duration(milliseconds: 50), () {
      setState(() {});
    });
  }

  @override
  Widget build(BuildContext context) => const Text('Late setState pending');
}

/// An app-level `HttpOverrides` installed after the binding that does not
/// chain to the previous one (the common certificate-pinning pattern).
class _SampleHttpOverrides extends HttpOverrides {
  @override
  HttpClient createHttpClient(SecurityContext? context) =>
      super.createHttpClient(context)..userAgent = 'tapwing-sample/1.0';
}

/// F8b trace viewer demo: HTTP requests to a server running inside the app
/// on the loopback interface (no host or internet services involved).
class NetworkScreen extends StatefulWidget {
  const NetworkScreen({super.key});

  @override
  State<NetworkScreen> createState() => _NetworkScreenState();
}

class _NetworkScreenState extends State<NetworkScreen> {
  HttpServer? _server;
  String _status = 'Network: starting';

  @override
  void initState() {
    super.initState();
    if (HttpOverrides.current is! _SampleHttpOverrides) {
      HttpOverrides.global = _SampleHttpOverrides();
    }
    unawaited(_start());
  }

  Future<void> _start() async {
    final HttpServer server = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    server.listen((HttpRequest req) async {
      await req.drain<void>();
      final HttpResponse res = req.response;
      switch (req.uri.path) {
        case '/api/products':
          await Future<void>.delayed(const Duration(milliseconds: 80));
          res.headers.contentType = ContentType.json;
          res.write(jsonEncode(<String, Object?>{
            'items': <String>['Coffee', 'Tea', 'Cocoa'],
          }));
        case '/api/login':
          res.headers.set('set-cookie', 'session=sample-session-secret');
          res.write('{"ok":true}');
        default:
          res.statusCode = HttpStatus.notFound;
          res.write('not found');
      }
      await res.close();
    });
    if (!mounted) {
      await server.close(force: true);
      return;
    }
    setState(() {
      _server = server;
      _status = 'Network: ready';
    });
  }

  @override
  void dispose() {
    unawaited(_server?.close(force: true));
    super.dispose();
  }

  Future<void> _request(
    String label,
    String method,
    Uri url, {
    Map<String, String> headers = const <String, String>{},
    String? body,
  }) async {
    setState(() => _status = 'Network: $label...');
    String result;
    // A client per request (like `package:http`'s top-level functions), so it
    // is created through the app's own HttpOverrides.
    final HttpClient client = HttpClient();
    try {
      final HttpClientRequest req = await client.openUrl(method, url);
      headers.forEach(req.headers.set);
      if (body != null) {
        req.headers.contentType = ContentType.json;
        req.write(body);
      }
      final HttpClientResponse res = await req.close();
      await res.drain<void>();
      result = '${res.statusCode}';
    } on Object {
      result = 'failed';
    } finally {
      client.close();
    }
    if (mounted) {
      setState(() => _status = 'Network: $label $result');
    }
  }

  Uri _url(String path, [Map<String, String>? query]) => Uri(
        scheme: 'http',
        host: '127.0.0.1',
        port: _server?.port ?? 9,
        path: path,
        queryParameters: query,
      );

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Network')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: <Widget>[
          Semantics(identifier: 'network_status', child: Text(_status)),
          const SizedBox(height: 8),
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: <Widget>[
              OutlinedButton(
                onPressed: () => _request(
                  'products',
                  'GET',
                  _url('/api/products', <String, String>{
                    'page': '1',
                    'access_token': 'sample-token-secret',
                  }),
                  headers: <String, String>{'authorization': 'Bearer sample-bearer-secret'},
                ),
                child: const Text('Fetch products'),
              ),
              OutlinedButton(
                onPressed: () => _request(
                  'login',
                  'POST',
                  _url('/api/login'),
                  headers: <String, String>{'x-api-key': 'sample-api-key-secret'},
                  body: '{"user":"demo","password":"sample-password-secret"}',
                ),
                child: const Text('Post login'),
              ),
              OutlinedButton(
                onPressed: () => _request('missing', 'GET', _url('/api/missing')),
                child: const Text('Fetch missing'),
              ),
              OutlinedButton(
                // Nothing listens on the discard port: connection refused.
                onPressed: () => _request(
                  'offline',
                  'GET',
                  Uri.parse('http://127.0.0.1:9/api/offline'),
                ),
                child: const Text('Fetch offline'),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

/// Where [AppStateScreen] keeps its state: one value in the platform
/// preferences (`NSUserDefaults` on iOS, `SharedPreferences` on Android) and one
/// file in the app's documents directory. Widget tests replace [appStateStore].
abstract class AppStateStore {
  Future<String?> readPreference();
  Future<void> writePreference(String value);
  Future<String?> readFile();
  Future<void> writeFile(String value);
}

class _PluginAppStateStore implements AppStateStore {
  static const String _key = 'tapwing_state_pref';
  static const String _fileName = 'tapwing_state.txt';

  Future<File> _file() async =>
      File('${(await getApplicationDocumentsDirectory()).path}/$_fileName');

  @override
  Future<String?> readPreference() async =>
      (await SharedPreferences.getInstance()).getString(_key);

  @override
  Future<void> writePreference(String value) async {
    await (await SharedPreferences.getInstance()).setString(_key, value);
  }

  @override
  Future<String?> readFile() async {
    final File file = await _file();
    return await file.exists() ? file.readAsString() : null;
  }

  @override
  Future<void> writeFile(String value) async {
    await (await _file()).writeAsString(value, flush: true);
  }
}

AppStateStore appStateStore = _PluginAppStateStore();

class AppStateScreen extends StatefulWidget {
  const AppStateScreen({super.key});

  @override
  State<AppStateScreen> createState() => _AppStateScreenState();
}

class _AppStateScreenState extends State<AppStateScreen> {
  String _preference = 'Preference: loading';
  String _file = 'File: loading';

  @override
  void initState() {
    super.initState();
    unawaited(_load());
  }

  Future<void> _load() async {
    final AppStateStore store = appStateStore;
    final String? preference = await store.readPreference();
    final String? file = await store.readFile();
    if (!mounted) return;
    setState(() {
      _preference = 'Preference: ${preference ?? 'none'}';
      _file = 'File: ${file ?? 'none'}';
    });
  }

  Future<void> _write() async {
    final AppStateStore store = appStateStore;
    await store.writePreference('saved-pref');
    await store.writeFile('saved-file');
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('App State')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: <Widget>[
          Semantics(identifier: 'state_preference', child: Text(_preference)),
          const SizedBox(height: 8),
          Semantics(identifier: 'state_file', child: Text(_file)),
          const SizedBox(height: 16),
          Semantics(
            identifier: 'state_write',
            child: FilledButton(
              onPressed: _write,
              child: const Text('Write state'),
            ),
          ),
        ],
      ),
    );
  }
}
