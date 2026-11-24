// A crash reporter installed between two RPCs replaces FlutterError.onError;
// the very next frame's errors must still reach Tapwing (re-wrap at drawFrame),
// and the reporter must still receive them (chaining).
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/diagnostics/app_issues.dart';
import 'package:tapwing_driver/src/diagnostics/error_hooks.dart';
import 'package:tapwing_driver/tapwing_driver.dart';

class _Binding extends WidgetsFlutterBinding with TapwingBindingMixin {
  _Binding._();

  static _Binding ensureInitialized() => _Binding._();
}

/// Mounts fine; throws in `build` once [armed] and marked dirty.
class _Flaky extends StatefulWidget {
  const _Flaky({super.key});

  @override
  State<_Flaky> createState() => _FlakyState();
}

class _FlakyState extends State<_Flaky> {
  static bool armed = false;

  void poke() => setState(() {});

  @override
  Widget build(BuildContext context) {
    if (armed) {
      throw StateError('boom in the next frame');
    }
    return const SizedBox();
  }
}

void main() {
  test('errors in the frame right after a reporter replaced the handler are recorded and chained', () async {
    final _Binding binding = _Binding.ensureInitialized();
    final AppIssueLog log = AppErrorHooks.instance.log..deliveryEnabled = true;
    final List<String> reporter = <String>[];

    final GlobalKey<_FlakyState> key = GlobalKey<_FlakyState>();
    runApp(Directionality(textDirection: TextDirection.ltr, child: _Flaky(key: key)));
    await Future<void>.delayed(Duration.zero); // the root attaches asynchronously
    binding.handleBeginFrame(Duration.zero);
    binding.handleDrawFrame();
    expect(log.takeUndelivered(), isEmpty);

    // The app installs its (non-chaining) reporter after the binding...
    FlutterError.onError = (FlutterErrorDetails d) =>
        reporter.add(d.exceptionAsString());

    // ...and the very next frame fails, with no RPC boundary in between.
    _FlakyState.armed = true;
    key.currentState!.poke();
    binding.handleBeginFrame(Duration.zero);
    binding.handleDrawFrame();

    expect(reporter, hasLength(1), reason: 'the reporter still gets the error');
    final List<AppIssue> issues = log.takeUndelivered();
    expect(issues, hasLength(1));
    expect(issues.single.kind, AppIssueKind.buildError);
    expect(issues.single.message, contains('boom in the next frame'));
  });
}
