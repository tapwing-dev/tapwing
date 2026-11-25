import '../selector/selector.dart';

/// Wire error codes returned in the `error.code` field of RPC responses.
///
/// The Rust CLI inspects these exact strings (for example `ELEMENT_NOT_FOUND`
/// prints `visibleTexts` in failure explanations).
abstract final class DriverErrorCode {
  /// Target selector did not match any visible element within the timeout.
  static const String elementNotFound = 'ELEMENT_NOT_FOUND';

  /// Assertion failed (e.g. `assertNotVisible` found the element still visible).
  static const String assertion = 'ASSERTION';

  /// Text input requested while no text-input client is attached.
  static const String noFocus = 'NO_FOCUS';

  /// Invalid command parameter (e.g. out-of-range point or negative coords).
  static const String invalidCommand = 'INVALID_COMMAND';

  /// Target element carried no text attribute to copy (`copyTextFrom`).
  static const String unableToCopyText = 'UNABLE_TO_COPY_TEXT';

  /// Unrecognized RPC method name.
  static const String unknownMethod = 'UNKNOWN_METHOD';

  /// Unhandled internal exception inside the driver.
  static const String internal = 'INTERNAL';
}

/// Structured RPC error thrown by command handlers and serialized as an error
/// response to the host CLI.
class DriverError implements Exception {
  /// Creates a driver error with wire [code], human-readable [message], and
  /// optional extra [details] merged into the JSON `error` object.
  DriverError(this.code, this.message, [this.details]);

  /// Creates an `ELEMENT_NOT_FOUND` error carrying up to 40 visible screen
  /// texts for diagnostic output on the host, plus optional
  /// [platformViewCoverage] (`0.0`–`1.0`) when a visible platform view is on
  /// screen.
  factory DriverError.elementNotFound(
    Selector selector,
    List<String> visibleTexts, {
    double? platformViewCoverage,
  }) => DriverError(
    DriverErrorCode.elementNotFound,
    'Element not found: ${selector.describe()}',
    <String, Object?>{
      'visibleTexts': visibleTexts,
      if (platformViewCoverage != null &&
          platformViewCoverage.isFinite &&
          platformViewCoverage > 0)
        'platformViewCoverage': platformViewCoverage.clamp(0.0, 1.0),
    },
  );

  /// Creates an `ASSERTION` error when `assertNotVisible` fails.
  factory DriverError.stillVisible(Selector selector) => DriverError(
    DriverErrorCode.assertion,
    '"${selector.describe()}" is visible',
  );

  /// Creates a `NO_FOCUS` error when text injection has no active client.
  factory DriverError.noFocus() => DriverError(
    DriverErrorCode.noFocus,
    'No focused text field to input text into',
  );

  /// Creates an `ASSERTION` error carrying Maestro's exact
  /// `MaestroException.HideKeyboardFailure` message (`Orchestra.kt:931-948`),
  /// raised by the opt-in strict iOS `hideKeyboard` emulation.
  factory DriverError.hideKeyboardFailure() => DriverError(
    DriverErrorCode.assertion,
    "Couldn't hide the keyboard. This can happen if the app uses a custom "
    "input or doesn't expose a standard dismiss action.",
  );

  /// Creates an `INVALID_COMMAND` error for invalid runtime arguments (e.g.
  /// element-relative point outside bounds).
  factory DriverError.invalidCommand(String message) =>
      DriverError(DriverErrorCode.invalidCommand, message);

  /// Creates an `UNABLE_TO_COPY_TEXT` error when `copyTextFrom` finds no text.
  factory DriverError.unableToCopyText(String message) =>
      DriverError(DriverErrorCode.unableToCopyText, message);

  /// Creates an `UNKNOWN_METHOD` error for an unrecognized RPC method.
  factory DriverError.unknownMethod(String method) =>
      DriverError(DriverErrorCode.unknownMethod, 'Unknown method $method');

  /// Machine-readable wire error code (see [DriverErrorCode]).
  final String code;

  /// Human-readable description displayed by the host CLI.
  final String message;

  /// Optional structured fields merged directly into the wire `error` object.
  final Map<String, Object?>? details;

  /// Serializes this error into the wire `error` payload expected by the CLI.
  Map<String, Object?> toJson() => <String, Object?>{
    'code': code,
    'message': message,
    ...?details,
  };

  @override
  String toString() => 'DriverError($code): $message';
}
