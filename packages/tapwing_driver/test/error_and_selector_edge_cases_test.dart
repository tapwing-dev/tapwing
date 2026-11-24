import 'package:flutter_test/flutter_test.dart';
import 'package:tapwing_driver/src/protocol/rpc_error.dart';
import 'package:tapwing_driver/src/selector/maestro_regex.dart';
import 'package:tapwing_driver/src/selector/selector.dart';

void main() {
  group(
    'DriverErrorCode wire strings (the Rust CLI matches these exactly)',
    () {
      test('never change spelling', () {
        expect(DriverErrorCode.elementNotFound, 'ELEMENT_NOT_FOUND');
        expect(DriverErrorCode.assertion, 'ASSERTION');
        expect(DriverErrorCode.noFocus, 'NO_FOCUS');
        expect(DriverErrorCode.invalidCommand, 'INVALID_COMMAND');
        expect(DriverErrorCode.unableToCopyText, 'UNABLE_TO_COPY_TEXT');
        expect(DriverErrorCode.unknownMethod, 'UNKNOWN_METHOD');
        expect(DriverErrorCode.internal, 'INTERNAL');
      });
    },
  );

  group('DriverError serialization', () {
    test(
      'elementNotFound carries the selector description and visible texts',
      () {
        final DriverError error = DriverError.elementNotFound(
          Selector(text: 'Log in', index: 1),
          <String>['Welcome back', 'Email'],
        );
        final Map<String, Object?> json = error.toJson();
        expect(json['code'], 'ELEMENT_NOT_FOUND');
        expect(
          json['message'],
          'Element not found: Text matching regex: Log in, Index: 1',
        );
        expect(json['visibleTexts'], <String>['Welcome back', 'Email']);
        expect(error.toString(), startsWith('DriverError(ELEMENT_NOT_FOUND):'));
      },
    );

    test('elementNotFound with no visible texts still serializes the list', () {
      final Map<String, Object?> json = DriverError.elementNotFound(
        Selector(id: 'missing'),
        const <String>[],
      ).toJson();
      expect(json['visibleTexts'], isEmpty);
      expect(json['message'], 'Element not found: Id matching regex: missing');
    });

    test('every factory maps to its wire code with no extra keys', () {
      final Map<DriverError, String> expected = <DriverError, String>{
        DriverError.stillVisible(Selector(text: 'Done')):
            DriverErrorCode.assertion,
        DriverError.noFocus(): DriverErrorCode.noFocus,
        DriverError.hideKeyboardFailure(): DriverErrorCode.assertion,
        DriverError.invalidCommand('bad point'): DriverErrorCode.invalidCommand,
        DriverError.unableToCopyText('nothing'):
            DriverErrorCode.unableToCopyText,
        DriverError.unknownMethod('frobnicate'): DriverErrorCode.unknownMethod,
      };
      for (final MapEntry<DriverError, String> e in expected.entries) {
        final Map<String, Object?> json = e.key.toJson();
        expect(json['code'], e.value, reason: e.key.message);
        expect(json.keys.toSet(), <String>{
          'code',
          'message',
        }, reason: e.key.message);
      }
      expect(
        DriverError.stillVisible(Selector(text: 'Done')).message,
        '"Text matching regex: Done" is visible',
      );
      expect(
        DriverError.unknownMethod('frobnicate').message,
        'Unknown method frobnicate',
      );
      expect(
        DriverError.noFocus().message,
        'No focused text field to input text into',
      );
      expect(
        DriverError.hideKeyboardFailure().message,
        startsWith("Couldn't hide the keyboard."),
      );
    });

    test('custom details are merged next to code and message', () {
      final Map<String, Object?> json = DriverError(
        'CUSTOM',
        'boom',
        <String, Object?>{
          'extra': 1,
          'nested': <String, Object?>{'a': true},
        },
      ).toJson();
      expect(json, <String, Object?>{
        'code': 'CUSTOM',
        'message': 'boom',
        'extra': 1,
        'nested': <String, Object?>{'a': true},
      });
    });

    test('is a throwable Exception whose message survives a catch', () {
      expect(
        () => throw DriverError.invalidCommand('x'),
        throwsA(
          isA<DriverError>()
              .having((DriverError e) => e.code, 'code', 'INVALID_COMMAND')
              .having((DriverError e) => e.message, 'message', 'x'),
        ),
      );
    });
  });

  group('MaestroRegex edge cases', () {
    test(
      'null never matches and the empty pattern only matches empty text',
      () {
        final MaestroRegex any = MaestroRegex('.*');
        expect(any.fullMatch(null), isFalse);
        expect(any.matchesText(null), isFalse);
        expect(any.fullMatch(''), isTrue);

        final MaestroRegex empty = MaestroRegex('');
        expect(empty.fullMatch(''), isTrue);
        expect(empty.fullMatch('x'), isFalse);
      },
    );

    test('is a full match, not a substring search', () {
      final MaestroRegex re = MaestroRegex('Log');
      expect(re.fullMatch('Log'), isTrue);
      expect(re.fullMatch('Log in'), isFalse);
      expect(re.fullMatch('Blog'), isFalse);
      expect(MaestroRegex('Log.*').fullMatch('Log in'), isTrue);
    });

    test('ignores case and lets dot match newlines', () {
      expect(MaestroRegex('welcome BACK').fullMatch('Welcome Back'), isTrue);
      expect(MaestroRegex('a.b').fullMatch('a\nb'), isTrue);
    });

    test('alternation is anchored as a whole group', () {
      final MaestroRegex re = MaestroRegex('cat|dog');
      expect(re.fullMatch('cat'), isTrue);
      expect(re.fullMatch('dog'), isTrue);
      expect(re.fullMatch('catdog'), isFalse);
      expect(re.fullMatch('hotdog'), isFalse);
    });

    test('patterns that do not compile fall back to an escaped literal', () {
      for (final String literal in <String>[
        'a)(b',
        '[unclosed',
        '(unclosed',
        '*leading',
        '+plus',
        r'\',
      ]) {
        final MaestroRegex re = MaestroRegex(literal);
        expect(re.fullMatch(literal), isTrue, reason: 'literal "$literal"');
        expect(re.matchesText(literal), isTrue, reason: 'literal "$literal"');
        expect(re.fullMatch('${literal}x'), isFalse, reason: literal);
      }
    });

    test('matchesText retries with newlines replaced by spaces', () {
      final MaestroRegex re = MaestroRegex('Product 1 € 3.00');
      expect(re.matchesText('Product 1\n€ 3.00'), isTrue);
      expect(re.fullMatch('Product 1\n€ 3.00'), isFalse);
      // Only newlines are rewritten, other whitespace is untouched.
      expect(re.matchesText('Product 1\t€ 3.00'), isFalse);
      // Two newlines become two spaces and do not collapse.
      expect(re.matchesText('Product 1\n\n€ 3.00'), isFalse);
    });

    test('literal equality wins even when the pattern is a valid regex', () {
      // `$5.00` is a valid regex that does not match itself ($ is an anchor).
      final MaestroRegex price = MaestroRegex(r'$5.00');
      expect(price.fullMatch(r'$5.00'), isFalse);
      expect(price.matchesText(r'$5.00'), isTrue);
      expect(price.matchesText(r'$5x00'), isFalse);
    });

    test(
      'unicode text, Czech diacritics and emoji match literally and by regex',
      () {
        expect(MaestroRegex('Přihlášení').fullMatch('přihlášení'), isTrue);
        expect(
          MaestroRegex('Žluťoučký kůň.*').fullMatch('Žluťoučký kůň úpěl'),
          isTrue,
        );
        expect(MaestroRegex('🚀 Start').matchesText('🚀 Start'), isTrue);
        expect(MaestroRegex('.').fullMatch('é'), isTrue);
      },
    );
  });

  group('ElementTrait.parse', () {
    test('accepts wire names and kebab-case in any case, with whitespace', () {
      expect(ElementTrait.parse('TEXT'), ElementTrait.text);
      expect(ElementTrait.parse(' square '), ElementTrait.square);
      expect(ElementTrait.parse('long-text'), ElementTrait.longText);
      expect(ElementTrait.parse('LONG_TEXT'), ElementTrait.longText);
      expect(ElementTrait.parse('Long-Text'), ElementTrait.longText);
    });

    test('rejects unknown traits with an ArgumentError naming the value', () {
      expect(
        () => ElementTrait.parse('shiny'),
        throwsA(
          isA<ArgumentError>().having(
            (ArgumentError e) => e.invalidValue,
            'invalidValue',
            'shiny',
          ),
        ),
      );
      expect(() => ElementTrait.parse(''), throwsArgumentError);
    });

    test('wire names and descriptions match Maestro', () {
      expect(ElementTrait.values.map((ElementTrait t) => t.wireName), <String>[
        'TEXT',
        'SQUARE',
        'LONG_TEXT',
      ]);
      expect(
        ElementTrait.values.map((ElementTrait t) => t.description),
        <String>['Has text', 'Is square', 'Has long text'],
      );
    });
  });

  group('Selector.fromJson / describe edge cases', () {
    test(
      'an empty map is an unconstrained selector with an empty description',
      () {
        final Selector sel = Selector.fromJson(<String, Object?>{});
        expect(sel.text, isNull);
        expect(sel.id, isNull);
        expect(sel.traits, isNull);
        expect(sel.describe(), isEmpty);
      },
    );

    test('numeric fields accept doubles from JSON and truncate to int', () {
      final Selector sel = Selector.fromJson(<String, Object?>{
        'index': 2.0,
        'width': 100.0,
        'height': 50,
        'tolerance': 3.0,
      });
      expect(sel.index, 2);
      expect(sel.width, 100);
      expect(sel.height, 50);
      expect(sel.tolerance, 3);
      expect(sel.describe(), contains('Size: 100x50'));
    });

    test(
      'size with only one dimension prints null for the other, like Maestro',
      () {
        expect(Selector(width: 10).describe(), 'Size: 10xnull');
        expect(Selector(height: 7).describe(), 'Size: nullx7');
      },
    );

    test('traits parse from JSON and describe in order', () {
      final Selector sel = Selector.fromJson(<String, Object?>{
        'traits': <Object?>['text', 'long-text'],
      });
      expect(sel.traits, <ElementTrait>[
        ElementTrait.text,
        ElementTrait.longText,
      ]);
      expect(sel.describe(), 'Has text, Has long text');
    });

    test('an unknown trait in JSON fails loudly instead of being dropped', () {
      expect(
        () => Selector.fromJson(<String, Object?>{
          'traits': <Object?>['nope'],
        }),
        throwsArgumentError,
      );
    });

    test('nested relative selectors parse recursively from untyped maps', () {
      final Selector sel = Selector.fromJson(<String, Object?>{
        'below': <Object?, Object?>{
          'text': 'Header',
          'above': <Object?, Object?>{'id': 'footer'},
        },
        'childOf': <String, Object?>{'id': 'list'},
        'containsChild': <String, Object?>{'text': 'Row'},
      });
      expect(sel.below?.text, 'Header');
      expect(sel.below?.above?.id, 'footer');
      expect(sel.childOf?.id, 'list');
      expect(sel.containsChild?.text, 'Row');
      expect(
        sel.describe(),
        'Below: Text matching regex: Header, Above: Id matching regex: footer, '
        'Contains child: Text matching regex: Row, '
        'Child of: Id matching regex: list',
      );
    });

    test('state flags describe both polarities', () {
      expect(Selector(enabled: false).describe(), 'Disabled');
      expect(Selector(selected: true).describe(), 'Selected');
      expect(Selector(selected: false).describe(), 'Not selected');
      expect(Selector(checked: true).describe(), 'Checked');
      expect(Selector(focused: false).describe(), 'Not focused');
      expect(Selector(css: 'div.a').describe(), 'CSS: div.a');
    });

    test('a text pattern with an invalid regex still describes verbatim', () {
      expect(
        Selector(text: '[unclosed').describe(),
        'Text matching regex: [unclosed',
      );
    });
  });
}
