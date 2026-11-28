// Tapwing entrypoint: production code stays untouched, the driver is a
// dev dependency. Build with:
//   flutter build apk --profile -t lib/main_tapwing.dart
// ignore_for_file: depend_on_referenced_packages
import 'dart:async';

import 'package:flutter/material.dart';
import 'package:tapwing_driver/tapwing_driver.dart';

import 'main.dart' as app;

Route<void> _instantRoute(Widget page) => PageRouteBuilder<void>(
  pageBuilder: (_, _, _) => page,
  transitionDuration: Duration.zero,
  reverseTransitionDuration: Duration.zero,
);

void main() {
  TapwingBinding.ensureInitialized();
  TapwingBinding.registerHooks(<String, TapwingHookHandler>{
    'seed_logged_in': (BuildContext context, Map<String, Object?> args) {
      final String email = (args['email'] as String?) ?? 'dev@tapwing.dev';
      unawaited(
        Navigator.of(context, rootNavigator: true).pushAndRemoveUntil<void>(
          _instantRoute(const app.ProductsScreen()),
          (_) => false,
        ),
      );
      return <String, Object?>{'screen': 'products', 'email': email};
    },
    'open_product': (BuildContext context, Map<String, Object?> args) {
      final Object? rawIndex = args['index'];
      final int index = rawIndex is num ? rawIndex.toInt() : 40;
      if (index < 1 || index > 100) {
        throw RangeError.range(index, 1, 100, 'index');
      }
      final NavigatorState nav = Navigator.of(context, rootNavigator: true);
      unawaited(
        nav.pushAndRemoveUntil<void>(
          _instantRoute(const app.ProductsScreen()),
          (_) => false,
        ),
      );
      unawaited(
        nav.push<void>(_instantRoute(app.DetailScreen(index: index))),
      );
      return <String, Object?>{'screen': 'detail', 'index': index};
    },
    'reset_to_login': (BuildContext context, Map<String, Object?> _) {
      unawaited(
        Navigator.of(context, rootNavigator: true).pushAndRemoveUntil<void>(
          _instantRoute(const app.LoginScreen()),
          (_) => false,
        ),
      );
      return <String, Object?>{'screen': 'login'};
    },
  });
  app.main();
}
