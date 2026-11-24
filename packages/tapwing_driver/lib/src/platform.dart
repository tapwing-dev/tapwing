import 'package:flutter/foundation.dart';

/// Target platform whose Maestro driver semantics Tapwing reproduces.
///
/// The platform decides how the UI hierarchy is composed and which unit
/// geometry is expressed in:
/// - [android]: Maestro `AndroidDriver` (UiAutomator dump of Flutter's Android
///   `AccessibilityBridge`), bounds in **physical pixels**.
/// - [ios]: Maestro `IOSDriver.mapViewHierarchy` (XCUITest snapshot of
///   Flutter's iOS `SemanticsObject`s), bounds in **logical points**.
enum DriverPlatform {
  /// Maestro Android semantics (physical pixels).
  android,

  /// Maestro iOS semantics (logical points).
  ios;

  /// Platform the app is running on (honors
  /// `debugDefaultTargetPlatformOverride` in tests).
  static DriverPlatform detect() =>
      defaultTargetPlatform == TargetPlatform.iOS ? ios : android;

  /// Parses a wire name (`"android"` / `"ios"`), or returns `null`.
  static DriverPlatform? tryParse(Object? raw) => switch (raw) {
    'android' => android,
    'ios' => ios,
    _ => null,
  };
}
