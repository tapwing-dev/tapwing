import 'dart:async';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/input/text_input.dart';

/// One `flutter/textinput` message that reached the (fake) engine.
class _EngineMessage {
  _EngineMessage(this.fromDriver, this.method, this.arguments);

  /// `true` for the driver's own `TextInput.setEditingState`; `false` for what
  /// the framework sent (`setClient`, `show`, a formatter's rewrite, ...).
  final bool fromDriver;
  final String method;
  final Object? arguments;

  String? get text => arguments is Map<Object?, Object?>
      ? (arguments! as Map<Object?, Object?>)['text'] as String?
      : null;
}

/// Records what the driver sends straight to the engine.
class _RecordingEngine implements BinaryMessenger {
  _RecordingEngine(this.log);

  final List<_EngineMessage> log;

  @override
  Future<ByteData?>? send(String channel, ByteData? message) {
    final MethodCall call = const JSONMethodCodec().decodeMethodCall(message);
    log.add(_EngineMessage(true, call.method, call.arguments));
    return null;
  }

  @override
  void setMessageHandler(String channel, MessageHandler? handler) {}

  @override
  // Required by Flutter's BinaryMessenger abstract interface.
  // ignore: deprecated_member_use
  Future<void> handlePlatformMessage(
    String channel,
    ByteData? data,
    ui.PlatformMessageResponseCallback? callback,
  ) async {}
}

class _Rig {
  _Rig(this.tester) {
    textInput.attach(_RecordingEngine(engine));
    tester.binding.defaultBinaryMessenger.setMockMessageHandler(
      SystemChannels.textInput.name,
      (ByteData? message) async {
        if (message != null) {
          textInput.onOutgoing(message);
          final MethodCall call = const JSONMethodCodec().decodeMethodCall(
            message,
          );
          engine.add(_EngineMessage(false, call.method, call.arguments));
        }
        return null;
      },
    );
  }

  final WidgetTester tester;
  final TextInputController textInput = TextInputController();
  final List<_EngineMessage> engine = <_EngineMessage>[];

  List<_EngineMessage> get driverSyncs => engine
      .where(
        (_EngineMessage m) =>
            m.fromDriver && m.method == 'TextInput.setEditingState',
      )
      .toList();

  /// The text the engine ends up holding: the last `setEditingState` from anyone.
  String? get engineText => engine
      .lastWhere((_EngineMessage m) => m.method == 'TextInput.setEditingState')
      .text;

  Future<void> focus() async {
    await tester.tap(find.byType(TextField));
    await tester.pump();
    expect(textInput.hasClient, isTrue);
  }

  Future<void> type(String text) async {
    await tester.runAsync(() => textInput.type(text));
    await tester.pump();
  }

  Future<void> erase(int count) async {
    await tester.runAsync(() => textInput.erase(count));
    await tester.pump();
  }

  void dispose() {
    tester.binding.defaultBinaryMessenger.setMockMessageHandler(
      SystemChannels.textInput.name,
      null,
    );
  }
}

Future<void> _pumpField(
  WidgetTester tester,
  TextEditingController controller, {
  List<TextInputFormatter>? formatters,
  ValueChanged<String>? onChanged,
  bool obscureText = false,
  int? maxLength,
}) {
  return tester.pumpWidget(
    MaterialApp(
      home: Scaffold(
        body: TextField(
          controller: controller,
          inputFormatters: formatters,
          onChanged: onChanged,
          obscureText: obscureText,
          maxLength: maxLength,
        ),
      ),
    ),
  );
}

/// Records every edit it is asked to format, then upper-cases it.
class _UpperCasing extends TextInputFormatter {
  final List<String> seen = <String>[];

  @override
  TextEditingValue formatEditUpdate(
    TextEditingValue oldValue,
    TextEditingValue newValue,
  ) {
    seen.add('${oldValue.text}>${newValue.text}');
    return newValue.copyWith(text: newValue.text.toUpperCase());
  }
}

void main() {
  testWidgets(
    'every keystroke still reaches the formatter and onChanged in its own event-loop turn, '
    'but the engine hears one message',
    (WidgetTester tester) async {
      final _Rig rig = _Rig(tester);
      final TextEditingController controller = TextEditingController();
      final _UpperCasing formatter = _UpperCasing()..seen.clear();
      final List<String> changed = <String>[];
      final List<int> turns = <int>[];
      int turn = 0;
      try {
        await _pumpField(
          tester,
          controller,
          formatters: <TextInputFormatter>[formatter],
          onChanged: (String v) {
            changed.add(v);
            turns.add(turn);
          },
        );
        await rig.focus();
        rig.engine.clear();

        // A counter that moves on once per event-loop turn (real async, not the test's fake clock).
        await tester.runAsync(() async {
          bool ticking = true;
          void tick() {
            turn++;
            if (ticking) Timer.run(tick);
          }

          Timer.run(tick);
          await rig.textInput.type('abc');
          ticking = false;
        });
        await tester.pump();

        expect(formatter.seen, <String>['>a', 'A>Ab', 'AB>ABc']);
        expect(changed, <String>['A', 'AB', 'ABC']);
        expect(
          turns.toSet(),
          hasLength(3),
          reason: 'each keystroke in its own turn: $turns',
        );
        expect(
          rig.driverSyncs,
          hasLength(1),
          reason: 'one setEditingState from the driver for 3 characters',
        );
      } finally {
        rig.dispose();
      }
    },
  );

  testWidgets(
    'a formatter that rewrites the value leaves the engine with the rewritten text',
    (WidgetTester tester) async {
      final _Rig rig = _Rig(tester);
      final TextEditingController controller = TextEditingController();
      try {
        await _pumpField(
          tester,
          controller,
          formatters: <TextInputFormatter>[_UpperCasing()],
        );
        await rig.focus();
        await rig.type('hello');
        expect(controller.text, 'HELLO');
        expect(
          rig.engineText,
          'HELLO',
          reason: 'the IME must not be left holding the unformatted "hello"',
        );
        expect(rig.textInput.editingState?['text'], 'HELLO');
      } finally {
        rig.dispose();
      }
    },
  );

  testWidgets(
    'an obscured field gets its real text, per keystroke, and the engine ends in step',
    (WidgetTester tester) async {
      final _Rig rig = _Rig(tester);
      final TextEditingController controller = TextEditingController();
      final List<String> changed = <String>[];
      try {
        await _pumpField(
          tester,
          controller,
          obscureText: true,
          onChanged: changed.add,
        );
        await rig.focus();
        await rig.type('secret123');
        expect(controller.text, 'secret123');
        expect(changed, hasLength(9));
        expect(changed.last, 'secret123');
        expect(rig.driverSyncs.map((_EngineMessage m) => m.text), <String?>[
          'secret123',
        ]);
        expect(rig.engineText, 'secret123');
      } finally {
        rig.dispose();
      }
    },
  );

  testWidgets(
    'maxLength drops the extra characters and the engine ends at the limit',
    (WidgetTester tester) async {
      final _Rig rig = _Rig(tester);
      final TextEditingController controller = TextEditingController();
      final List<String> changed = <String>[];
      try {
        await _pumpField(
          tester,
          controller,
          maxLength: 5,
          onChanged: changed.add,
        );
        await rig.focus();
        await rig.type('abcdefgh');
        expect(controller.text, 'abcde');
        // Characters past the limit are rejected by the formatter, so only five edits are reported.
        expect(changed, <String>['a', 'ab', 'abc', 'abcd', 'abcde']);
        expect(rig.engineText, 'abcde');
      } finally {
        rig.dispose();
      }
    },
  );

  testWidgets(
    'erase and later typing continue from the state the engine was told',
    (WidgetTester tester) async {
      final _Rig rig = _Rig(tester);
      final TextEditingController controller = TextEditingController();
      final List<String> changed = <String>[];
      try {
        await _pumpField(tester, controller, onChanged: changed.add);
        await rig.focus();
        await rig.type('hello');
        rig.engine.clear();
        changed.clear();

        await rig.erase(3);
        expect(controller.text, 'he');
        expect(changed, <String>['hell', 'hel', 'he']);
        expect(rig.driverSyncs.map((_EngineMessage m) => m.text), <String?>[
          'he',
        ]);

        await rig.type('xy');
        expect(controller.text, 'hexy');
        expect(rig.engineText, 'hexy');
        expect(rig.textInput.editingState?['selectionBase'], 4);
      } finally {
        rig.dispose();
      }
    },
  );

  testWidgets(
    'typing nothing, or into a field that went away, sends nothing to the engine',
    (WidgetTester tester) async {
      final _Rig rig = _Rig(tester);
      final TextEditingController controller = TextEditingController();
      try {
        await _pumpField(tester, controller);
        await rig.focus();
        rig.engine.clear();

        await rig.type('');
        await rig.erase(2);
        expect(rig.driverSyncs, isEmpty);

        // The field loses its client while the driver is typing.
        Object? error;
        await tester.runAsync(() async {
          final Future<void> typing = rig.textInput.type('abc');
          rig.textInput.onOutgoing(
            const JSONMethodCodec().encodeMethodCall(
              const MethodCall('TextInput.clearClient'),
            ),
          );
          try {
            await typing;
          } catch (e) {
            error = e;
          }
        });
        expect(error, isA<StateError>());
        expect(
          rig.driverSyncs,
          isEmpty,
          reason: 'no client left to tell the engine about',
        );
      } finally {
        rig.dispose();
      }
    },
  );
}
