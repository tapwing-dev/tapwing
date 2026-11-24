/// Port of Maestro 2.12.1's `Regex(pattern, {IGNORE_CASE, DOT_MATCHES_ALL})`
/// evaluated as a full match, with `toRegexSafe` literal fallback.
///
/// Maestro matches text and resource-id fields against full-string regular
/// expressions. Patterns that fail to compile standalone fall back to an
/// escaped literal so user strings containing metacharacters like `"a)(b"`
/// still match reliably.
class MaestroRegex {
  /// Compiles [pattern] using Maestro's case-insensitive dotAll rules.
  MaestroRegex(this.pattern) : _re = _compile(pattern);

  /// Original unanchored pattern string from the selector.
  final String pattern;

  final RegExp _re;

  static RegExp _compile(String pattern) {
    try {
      // Validate the pattern standalone first so unbalanced parentheses like
      // "a)(b" fail and fall back to literal matching rather than pairing
      // with the outer `^(?:...)$` group.
      RegExp(pattern, caseSensitive: false, dotAll: true);
      return RegExp('^(?:$pattern)\$', caseSensitive: false, dotAll: true);
    } on FormatException {
      return RegExp(
        '^${RegExp.escape(pattern)}\$',
        caseSensitive: false,
        dotAll: true,
      );
    }
  }

  /// Returns `true` if [value] is non-null and fully matches the compiled regex.
  bool fullMatch(String? value) => value != null && _re.hasMatch(value);

  /// Port of Maestro `Filters.textMatches`: accepts a full regex match or
  /// exact literal equality against [value], and also retries with `'\n'`
  /// replaced by `' '` (e.g. matching multi-line `ListTile` labels).
  bool matchesText(String? value) {
    if (value == null) return false;
    if (_re.hasMatch(value) || pattern == value) return true;
    if (!value.contains('\n')) return false;
    final String stripped = value.replaceAll('\n', ' ');
    return _re.hasMatch(stripped) || pattern == stripped;
  }
}
