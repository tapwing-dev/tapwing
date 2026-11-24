import 'package:flutter/services.dart';

/// Counts system Back deliveries from the engine to the framework.
///
/// On Android the host CLI injects a real `KEYCODE_BACK` so the OS decides
/// where Back goes, exactly like Maestro: with
/// `android:enableOnBackInvokedCallback="true"` the engine only forwards Back
/// while the framework reports it can handle it
/// (`SystemNavigator.setFrameworkHandlesBack`); otherwise Android finishes or
/// backgrounds the activity. The driver cannot know the manifest, so it only
/// observes what arrives: `popRoute` on `flutter/navigation` or
/// `commitBackGesture` on `flutter/backgesture` (predictive back on API 34+,
/// which the framework turns into `handlePopRoute`).
///
/// Fed by `ObservingBinaryMessenger`, which wraps the framework's own
/// handlers, so it sees each delivery before any `WidgetsBindingObserver`
/// (Flutter calls observers in registration order and stops at the first one
/// that handles the pop, so an observer added by the driver would never see
/// it).
class BackSignals {
  /// Creates an independent counter (tests); the app uses [instance].
  BackSignals();

  /// Shared instance fed by the binding's messenger.
  static final BackSignals instance = BackSignals();

  int _deliveries = 0;

  /// Number of Back deliveries seen so far.
  int get deliveries => _deliveries;

  /// Whether [channel] can carry a Back delivery.
  static bool observes(String channel) =>
      channel == SystemChannels.navigation.name ||
      channel == SystemChannels.backGesture.name;

  /// Records an incoming platform message on [channel]; only `popRoute` and
  /// `commitBackGesture` count. Malformed messages are ignored (the
  /// framework's own handler still receives them).
  void onIncoming(String channel, ByteData? data) {
    if (data == null) return;
    try {
      if (channel == SystemChannels.navigation.name) {
        final MethodCall call = const JSONMethodCodec().decodeMethodCall(data);
        if (call.method == 'popRoute') _deliveries++;
      } else if (channel == SystemChannels.backGesture.name) {
        final MethodCall call = const StandardMethodCodec().decodeMethodCall(
          data,
        );
        if (call.method == 'commitBackGesture') _deliveries++;
      }
    } on Object {
      // Not a method call this counter understands.
    }
  }
}
