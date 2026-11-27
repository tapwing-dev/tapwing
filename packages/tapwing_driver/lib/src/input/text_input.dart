import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/services.dart';

import '../diagnostics/text_input_log.dart';
import '../idle/channel_tracker.dart';
import 'back_signals.dart';

/// Observes `flutter/textinput` messages between the framework and the engine
/// to track the active text-input client, editing state, and soft-keyboard
/// requests, and injects edits at the engine boundary like an IME.
///
/// Edits are delivered via `ui.channelBuffers.push`
/// (`TextInputClient.updateEditingState` or
/// `TextInputClient.updateEditingStateWithDeltas` when `enableDeltaModel` is
/// configured) so `TextInputFormatter`s and `onChanged` callbacks run normally,
/// and mirrored to the native IME with one `TextInput.setEditingState` when
/// [type] or [erase] finishes.
class TextInputController {
  /// Creates a controller with its own diagnostic [log].
  TextInputController({TextInputLog? log}) : log = log ?? TextInputLog();

  /// Shared singleton wired into [ObservingBinaryMessenger] by default.
  static final TextInputController instance = TextInputController();

  /// Ring-buffer log of recent `flutter/textinput` messages and tap markers.
  final TextInputLog log;

  int? _clientId;
  String? _inputAction;
  String? _inputTypeName;
  bool _readOnly = false;
  bool _enableDeltaModel = false;
  Map<String, Object?>? _editingState;
  bool _keyboardRequested = false;
  final Stopwatch _sinceKeyboard = Stopwatch();
  BinaryMessenger? _engine;

  /// Active Flutter text-input client id, or `null` when no text field is
  /// focused.
  int? get clientId => _clientId;

  /// Configured `inputAction` string from `TextInput.setClient` (e.g.
  /// `'TextInputAction.done'`), or `null` when unavailable.
  String? get inputAction => _inputAction;

  /// Whether the active client suppresses the soft keyboard (`readOnly` or
  /// `TextInputType.none`), so a missing inset change must not be mistaken for
  /// a hardware-keyboard device.
  bool get suppressesSoftKeyboard =>
      _readOnly || _inputTypeName == 'TextInputType.none';

  /// Whether the active client opted into the delta editing model
  /// (`TextInputConfiguration.enableDeltaModel`).
  bool get enableDeltaModel => _enableDeltaModel;

  /// Last observed or committed editing state map for the active client.
  Map<String, Object?>? get editingState => _editingState;

  /// Whether the framework last asked the platform to show the soft keyboard.
  bool get keyboardRequested => _keyboardRequested;

  /// Whether a text-input client is currently attached.
  bool get hasClient => _clientId != null;

  /// Elapsed time since the last keyboard show/hide state transition, or one
  /// day if no transition has been observed yet.
  Duration get sinceKeyboardChange => _sinceKeyboard.isRunning
      ? _sinceKeyboard.elapsed
      : const Duration(days: 1);

  /// Records the underlying engine [BinaryMessenger] used to mirror editing
  /// state and hide-keyboard requests to the platform IME.
  void attach(BinaryMessenger engineMessenger) {
    _engine = engineMessenger;
  }

  /// Records an outgoing `flutter/textinput` message from the framework to the
  /// engine and updates tracked client/keyboard state.
  void onOutgoing(ByteData message) {
    log.record('out ${TextInputLog.describeCall(message)}');
    final MethodCall call;
    try {
      call = const JSONMethodCodec().decodeMethodCall(message);
    } catch (_) {
      return;
    }
    switch (call.method) {
      case 'TextInput.setClient':
        if (call.arguments case [final int id, ...]) {
          final List<Object?> args = call.arguments as List<Object?>;
          _clientId = id;
          final Object? config = args.length > 1 ? args[1] : null;
          if (config is Map<Object?, Object?>) {
            _inputAction = config['inputAction'] as String?;
            _readOnly = config['readOnly'] == true;
            _enableDeltaModel = config['enableDeltaModel'] == true;
            final Object? inputType = config['inputType'];
            _inputTypeName = inputType is Map<Object?, Object?>
                ? inputType['name'] as String?
                : null;
          } else {
            _inputAction = null;
            _readOnly = false;
            _enableDeltaModel = false;
            _inputTypeName = null;
          }
        }
      case 'TextInput.clearClient':
        _clientId = null;
        _inputAction = null;
        _inputTypeName = null;
        _readOnly = false;
        _enableDeltaModel = false;
        _editingState = null;
        _setKeyboardRequested(false);
      case 'TextInput.setEditingState':
        if (call.arguments case final Map<Object?, Object?> map) {
          _editingState = map.cast<String, Object?>();
        }
      case 'TextInput.show':
        _setKeyboardRequested(true);
      case 'TextInput.hide':
        _setKeyboardRequested(false);
    }
  }

  /// Records an incoming `flutter/textinput` message from the engine to the
  /// framework.
  void onIncoming(ByteData message) {
    // Two spaces after "in" align with "out " and "tap " in diagnostic logs.
    log.record('in  ${TextInputLog.describeCall(message)}');
  }

  /// Records a tap location in the diagnostic log for focus debugging.
  void logTap(ui.Offset position) {
    log.record('tap ${position.dx.round()},${position.dy.round()}');
  }

  /// Captures an [ImeDiagnostics] payload containing entries recorded since
  /// [mark].
  ImeDiagnostics diagnosticsSince(
    int mark, {
    int? usedClient,
    bool noClient = false,
  }) => ImeDiagnostics(
    usedClient: usedClient,
    clientId: _clientId,
    keyboard: _keyboardRequested,
    log: log.since(mark),
    noClient: noClient,
  );

  /// Types [text] one Unicode rune at a time at the current selection.
  ///
  /// Yields a zero-duration timer after each rune so per-character formatters
  /// and listeners observe each edit sequentially. The native IME is told the
  /// final state once at the end, not after every rune: the engine handles that
  /// message on the platform thread, so a message per rune delays every yield.
  Future<void> type(String text) async {
    final int? client = _clientId;
    bool edited = false;
    try {
      for (final int rune in text.runes) {
        _replaceSelection(String.fromCharCode(rune));
        edited = true;
        await Future<void>.delayed(Duration.zero);
      }
    } finally {
      if (edited) _syncEngine(client);
    }
  }

  /// Deletes backward [count] times from the current selection (Maestro
  /// `eraseText`), stepping cleanly over UTF-16 surrogate pairs.
  ///
  /// Like [type], tells the native IME the final state once at the end.
  Future<void> erase(int count) async {
    final int? client = _clientId;
    bool edited = false;
    try {
      for (int i = 0; i < count; i++) {
        final Map<String, Object?> state = _requireState();
        final String text = (state['text'] as String?) ?? '';
        final (int start, int end) = _normalizedSelection(state, text.length);
        if (start == end && start == 0) return;
        final int from = start == end
            ? _prevCodePointBoundary(text, start)
            : start;
        _commit(
          oldText: text,
          deltaText: '',
          deltaStart: from,
          deltaEnd: end,
          newText: text.replaceRange(from, end, ''),
          cursor: from,
        );
        edited = true;
        await Future<void>.delayed(Duration.zero);
      }
    } finally {
      if (edited) _syncEngine(client);
    }
  }

  /// Dispatches the active text field's IME action (`pressKey: Enter`).
  ///
  /// For multiline fields (`TextInputAction.newline`), inserts a newline via
  /// [type]; otherwise pushes `TextInputClient.performAction` through
  /// `ui.channelBuffers.push` using the field's configured [inputAction]
  /// (defaulting to `'TextInputAction.done'`).
  Future<void> performAction() async {
    if (_clientId == null) {
      throw StateError('No focused text field');
    }
    final String action = _inputAction ?? 'TextInputAction.done';
    if (action == 'TextInputAction.newline') {
      await type('\n');
      return;
    }
    ui.channelBuffers.push(
      SystemChannels.textInput.name,
      const JSONMethodCodec().encodeMethodCall(
        MethodCall('TextInputClient.performAction', <Object?>[
          _clientId,
          action,
        ]),
      ),
      (ByteData? _) {},
    );
    await Future<void>.delayed(Duration.zero);
  }

  /// Asks the platform to hide the soft keyboard, sending the exact same
  /// `TextInput.hide` message the framework sends.
  void hideKeyboard() {
    _engine?.send(
      SystemChannels.textInput.name,
      const JSONMethodCodec().encodeMethodCall(
        const MethodCall('TextInput.hide'),
      ),
    );
    _setKeyboardRequested(false);
  }

  void _setKeyboardRequested(bool shown) {
    if (_keyboardRequested == shown) return;
    _keyboardRequested = shown;
    _sinceKeyboard
      ..reset()
      ..start();
  }

  Map<String, Object?> _requireState() {
    if (_clientId == null) {
      throw StateError('No focused text field');
    }
    return _editingState ??
        <String, Object?>{'text': '', 'selectionBase': 0, 'selectionExtent': 0};
  }

  (int, int) _normalizedSelection(Map<String, Object?> state, int textLength) {
    int base = (state['selectionBase'] as int?) ?? textLength;
    int extent = (state['selectionExtent'] as int?) ?? textLength;
    if (base < 0) base = textLength;
    if (extent < 0) extent = textLength;
    base = base.clamp(0, textLength);
    extent = extent.clamp(0, textLength);
    return base < extent ? (base, extent) : (extent, base);
  }

  void _replaceSelection(String insert) {
    final Map<String, Object?> state = _requireState();
    final String text = (state['text'] as String?) ?? '';
    final (int start, int end) = _normalizedSelection(state, text.length);
    _commit(
      oldText: text,
      deltaText: insert,
      deltaStart: start,
      deltaEnd: end,
      newText: text.replaceRange(start, end, insert),
      cursor: start + insert.length,
    );
  }

  void _commit({
    required String oldText,
    required String deltaText,
    required int deltaStart,
    required int deltaEnd,
    required String newText,
    required int cursor,
  }) {
    final Map<String, Object?> value = <String, Object?>{
      'text': newText,
      'selectionBase': cursor,
      'selectionExtent': cursor,
      'selectionAffinity': 'TextAffinity.downstream',
      'selectionIsDirectional': false,
      'composingBase': -1,
      'composingExtent': -1,
    };
    _editingState = value;
    final MethodCall clientCall = _enableDeltaModel
        ? MethodCall('TextInputClient.updateEditingStateWithDeltas', <Object?>[
            _clientId,
            <String, Object?>{
              'deltas': <Map<String, Object?>>[
                <String, Object?>{
                  'oldText': oldText,
                  'deltaText': deltaText,
                  'deltaStart': deltaStart,
                  'deltaEnd': deltaEnd,
                  'selectionBase': cursor,
                  'selectionExtent': cursor,
                  'selectionAffinity': 'TextAffinity.downstream',
                  'selectionIsDirectional': false,
                  'composingBase': -1,
                  'composingExtent': -1,
                },
              ],
            },
          ])
        : MethodCall('TextInputClient.updateEditingState', <Object?>[
            _clientId,
            value,
          ]);
    ui.channelBuffers.push(
      SystemChannels.textInput.name,
      const JSONMethodCodec().encodeMethodCall(clientCall),
      (ByteData? _) {},
    );
  }

  /// Keeps the native IME's model in sync with the edits [type] or [erase]
  /// made, as the framework does after a programmatic editing change.
  ///
  /// Sends [_editingState], not the last value this controller committed: when
  /// an input formatter rewrote a keystroke the framework already told the
  /// platform the rewritten value (observed in [onOutgoing]), and that is the
  /// one the IME must end with. Nothing is sent when the field the edits went
  /// to is gone.
  void _syncEngine(int? client) {
    final Map<String, Object?>? state = _editingState;
    if (client == null || client != _clientId || state == null) return;
    _engine?.send(
      SystemChannels.textInput.name,
      const JSONMethodCodec().encodeMethodCall(
        MethodCall('TextInput.setEditingState', state),
      ),
    );
  }

  static int _prevCodePointBoundary(String text, int index) {
    if (index <= 0) return 0;
    // Step over a UTF-16 trailing surrogate so emoji/astral runes erase as one
    // character rather than leaving a lone high surrogate.
    final int codeUnit = text.codeUnitAt(index - 1);
    if (index >= 2 && codeUnit >= 0xDC00 && codeUnit <= 0xDFFF) {
      return index - 2;
    }
    return index - 1;
  }
}

/// Transparent [BinaryMessenger] wrapper that lets [TextInputController]
/// observe `flutter/textinput` messages in both directions, [BackSignals]
/// observe Back deliveries, and [ChannelTracker] track pending non-system
/// platform-channel replies without altering them.
class ObservingBinaryMessenger implements BinaryMessenger {
  /// Wraps [_inner] and attaches it to [controller] (defaults to
  /// [TextInputController.instance]), [backSignals] (defaults to
  /// [BackSignals.instance]), and [channelTracker] (defaults to
  /// [ChannelTracker.instance]).
  ObservingBinaryMessenger(
    this._inner, {
    TextInputController? controller,
    BackSignals? backSignals,
    ChannelTracker? channelTracker,
  }) : _controller = controller ?? TextInputController.instance,
       _backSignals = backSignals ?? BackSignals.instance,
       _channelTracker = channelTracker ?? ChannelTracker.instance {
    _controller.attach(_inner);
  }

  final BinaryMessenger _inner;
  final TextInputController _controller;
  final BackSignals _backSignals;
  final ChannelTracker _channelTracker;

  @override
  Future<ByteData?>? send(String channel, ByteData? message) {
    if (channel == SystemChannels.textInput.name && message != null) {
      _controller.onOutgoing(message);
    }
    final Future<ByteData?>? reply = _inner.send(channel, message);
    if (reply != null && _channelTracker.tracks(channel)) {
      _channelTracker.track(channel, reply);
    }
    return reply;
  }

  @override
  void setMessageHandler(String channel, MessageHandler? handler) {
    if (channel == SystemChannels.textInput.name && handler != null) {
      _inner.setMessageHandler(channel, (ByteData? data) {
        if (data != null) {
          _controller.onIncoming(data);
        }
        return handler(data);
      });
      return;
    }
    if (BackSignals.observes(channel) && handler != null) {
      _inner.setMessageHandler(channel, (ByteData? data) {
        _backSignals.onIncoming(channel, data);
        return handler(data);
      });
      return;
    }
    _inner.setMessageHandler(channel, handler);
  }

  @override
  // Required by Flutter's BinaryMessenger abstract interface.
  // ignore: deprecated_member_use
  Future<void> handlePlatformMessage(
    String channel,
    ByteData? data,
    ui.PlatformMessageResponseCallback? callback,
  ) =>
      // Delegate directly to the wrapped BinaryMessenger implementation.
      // ignore: deprecated_member_use
      _inner.handlePlatformMessage(channel, data, callback);
}
