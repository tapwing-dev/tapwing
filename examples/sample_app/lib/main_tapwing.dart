// Tapwing entrypoint: production code stays untouched, the driver is a
// dev dependency. Build with:
//   flutter build apk --profile -t lib/main_tapwing.dart
// ignore_for_file: depend_on_referenced_packages
import 'package:tapwing_driver/tapwing_driver.dart';

import 'main.dart' as app;

void main() {
  TapwingBinding.ensureInitialized();
  app.main();
}
