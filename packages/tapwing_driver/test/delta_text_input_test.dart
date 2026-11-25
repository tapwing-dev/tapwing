import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/idle/http_tracker.dart';
import 'package:tapwing_driver/src/idle/idle_detector.dart';
import 'package:tapwing_driver/src/input/pointer_input.dart';
import 'package:tapwing_driver/src/input/text_input.dart';
import 'package:tapwing_driver/src/server/command_handlers.dart';
import 'package:tapwing_driver/src/server/driver_context.dart';

/// Minimal `DeltaTextInputClient` editor widget mirroring rich-text editors
/// (`enableDeltaModel: true`) that apply `TextEditingDelta`s rather than full
/// `TextEditingValue` snapshots.
class _DeltaEditor extends StatefulWidget {
  const _DeltaEditor({required this.controller});

  final _DeltaEditorController controller;

  @override
  State<_DeltaEditor> createState() => _DeltaEditorState();
}

class _DeltaEditorController extends ChangeNotifier {
  _DeltaEditorController({String initialText = ''})
    : value = TextEditingValue(
        text: initialText,
        selection: TextSelection.collapsed(offset: initialText.length),
      );

  TextEditingValue value;
  final List<TextEditingDelta> receivedDeltas = <TextEditingDelta>[];
  int legacyUpdateCalls = 0;

  void setSelection(TextSelection selection) {
    value = value.copyWith(selection: selection);
    notifyListeners();
  }
}

class _DeltaEditorState extends State<_DeltaEditor>
    with TextInputClient, DeltaTextInputClient {
  final FocusNode _focusNode = FocusNode();
  TextInputConnection? _connection;

  @override
  void initState() {
    super.initState();
    _focusNode.addListener(_onFocusChanged);
    widget.controller.addListener(_syncToPlatform);
  }

  @override
  void dispose() {
    widget.controller.removeListener(_syncToPlatform);
    _focusNode.removeListener(_onFocusChanged);
    _connection?.close();
    _focusNode.dispose();
    super.dispose();
  }

  void _onFocusChanged() {
    if (_focusNode.hasFocus) {
      _connection ??= TextInput.attach(
        this,
        const TextInputConfiguration(enableDeltaModel: true),
      );
      _connection!
        ..setEditingState(widget.controller.value)
        ..show();
    } else {
      _connection?.close();
      _connection = null;
    }
  }

  void _syncToPlatform() {
    _connection?.setEditingState(widget.controller.value);
    setState(() {});
  }

  @override
  TextEditingValue get currentTextEditingValue => widget.controller.value;

  @override
  AutofillScope? get currentAutofillScope => null;

  @override
  void updateEditingValue(TextEditingValue value) {
    // Delta-model clients ignore non-delta updates (just like Flutter's
    // DeltaTextInputClient example), which caused inputText to be a no-op
    // before enableDeltaModel support.
    widget.controller.legacyUpdateCalls++;
  }

  @override
  void updateEditingValueWithDeltas(List<TextEditingDelta> textEditingDeltas) {
    widget.controller.receivedDeltas.addAll(textEditingDeltas);
    TextEditingValue next = widget.controller.value;
    for (final TextEditingDelta delta in textEditingDeltas) {
      next = delta.apply(next);
    }
    widget.controller.value = next;
    _connection?.setEditingState(next);
    setState(() {});
  }

  @override
  void performAction(TextInputAction action) {}

  @override
  void performPrivateCommand(String action, Map<String, dynamic> data) {}

  @override
  void updateFloatingCursor(RawFloatingCursorPoint point) {}

  @override
  void showAutocorrectionPromptRect(int start, int end) {}

  @override
  void connectionClosed() {
    _connection = null;
  }

  @override
  Widget build(BuildContext context) {
    return GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: _focusNode.requestFocus,
      child: Focus(
        focusNode: _focusNode,
        child: Semantics(
          identifier: 'delta_editor',
          textField: true,
          value: widget.controller.value.text,
          child: Container(
            width: 300,
            height: 60,
            alignment: Alignment.centerLeft,
            padding: const EdgeInsets.symmetric(horizontal: 12),
            child: Text(widget.controller.value.text),
          ),
        ),
      ),
    );
  }
}

void main() {
  Future<T> runDriver<T>(WidgetTester tester, Future<T> future) async {
    T? result;
    Object? err;
    StackTrace? st;
    bool done = false;
    unawaited(
      future.then(
        (T value) {
          result = value;
          done = true;
        },
        onError: (Object e, StackTrace s) {
          err = e;
          st = s;
          done = true;
        },
      ),
    );
    for (int i = 0; i < 400 && !done; i++) {
      await tester.pump(const Duration(milliseconds: 16));
    }
    expect(done, isTrue, reason: 'Driver RPC did not complete');
    if (err != null) Error.throwWithStackTrace(err!, st!);
    return result as T;
  }

  testWidgets(
    'inputText and eraseText drive DeltaTextInputClient (enableDeltaModel: true)',
    (WidgetTester tester) async {
      final SemanticsHandle handle = tester.ensureSemantics();
      final _DeltaEditorController editor = _DeltaEditorController();
      final TextInputController textInput = TextInputController();

      tester.binding.defaultBinaryMessenger.setMockMessageHandler(
        SystemChannels.textInput.name,
        (ByteData? message) async {
          if (message != null) textInput.onOutgoing(message);
          return null;
        },
      );

      try {
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: Center(child: _DeltaEditor(controller: editor)),
            ),
          ),
        );

        final CommandDispatcher dispatcher = CommandDispatcher(
          DriverContext(
            idle: IdleDetector(
              httpTracker: HttpTracker(),
              textInput: textInput,
              readBottomInset: () => 0,
              waitForFirstFrame: (_) async {},
            ),
            pointer: PointerInput(),
            textInput: textInput,
          ),
        );

        // Tap to focus and attach the DeltaTextInputClient.
        await runDriver<Object?>(
          tester,
          dispatcher.dispatch('tapOn', <String, Object?>{
            'selector': <String, Object?>{'id': 'delta_editor'},
            'timeoutMs': 500,
            'settleTimeoutMs': 0,
          }),
        );
        expect(textInput.hasClient, isTrue);
        expect(textInput.enableDeltaModel, isTrue);

        // 1. Type text including an astral emoji -> TextEditingDeltaInsertion.
        await runDriver<Object?>(
          tester,
          dispatcher.dispatch('inputText', <String, Object?>{
            'text': 'hi😀!',
            'settleTimeoutMs': 0,
          }),
        );
        expect(editor.value.text, 'hi😀!');
        expect(editor.legacyUpdateCalls, 0);
        expect(
          editor.receivedDeltas.whereType<TextEditingDeltaInsertion>(),
          hasLength(4),
        );

        // 2. Erase backward across '!' and the surrogate-pair emoji ->
        // TextEditingDeltaDeletion.
        await runDriver<Object?>(
          tester,
          dispatcher.dispatch('eraseText', <String, Object?>{
            'count': 2,
            'settleTimeoutMs': 0,
          }),
        );
        expect(editor.value.text, 'hi');
        expect(
          editor.receivedDeltas.whereType<TextEditingDeltaDeletion>(),
          hasLength(2),
        );

        // 3. Non-collapsed selection replacement -> TextEditingDeltaReplacement
        // on the first typed rune, then insertion on the second.
        editor.setSelection(
          const TextSelection(baseOffset: 0, extentOffset: 2),
        );
        await tester.pump();
        await runDriver<Object?>(
          tester,
          dispatcher.dispatch('inputText', <String, Object?>{
            'text': 'yo',
            'settleTimeoutMs': 0,
          }),
        );
        expect(editor.value.text, 'yo');
        expect(
          editor.receivedDeltas.whereType<TextEditingDeltaReplacement>(),
          hasLength(1),
        );

        // 4. Clearing the client resets enableDeltaModel back to false.
        textInput.onOutgoing(
          const JSONMethodCodec().encodeMethodCall(
            const MethodCall('TextInput.clearClient'),
          ),
        );
        expect(textInput.enableDeltaModel, isFalse);
      } finally {
        tester.binding.defaultBinaryMessenger.setMockMessageHandler(
          SystemChannels.textInput.name,
          null,
        );
        editor.dispose();
        handle.dispose();
      }
    },
  );
}
