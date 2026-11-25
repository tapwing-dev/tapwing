//! Dart entrypoint inspection and `lib/main_tapwing.dart` generation.

use std::path::{Component, Path};

/// Signature properties of a Dart top-level `main` function relevant for forwarding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MainSignature {
    /// True when `app.main(...)` returns a `Future` (or `FutureOr`) and should be `await`ed.
    pub returns_future: bool,
    /// True when `app.main` accepts a `List<String>` argument parameter.
    pub takes_args: bool,
}

/// Zone-creating wrapper pattern detected in a Dart entrypoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneWrapper {
    /// `runZonedGuarded(...)`
    RunZonedGuarded,
    /// `runZoned(...)`
    RunZoned,
    /// `SentryFlutter.init(..., appRunner: ...)`
    SentryAppRunner,
}

impl ZoneWrapper {
    /// Short human-readable label for CLI warnings and generated comments.
    pub fn label(self) -> &'static str {
        match self {
            Self::RunZonedGuarded => "runZonedGuarded",
            Self::RunZoned => "runZoned",
            Self::SentryAppRunner => "SentryFlutter.init(..., appRunner: ...)",
        }
    }
}

/// Detects zone-creating wrapper calls in `dart_source`, ignoring comments and string literals.
pub fn detect_zone_wrappers(dart_source: &str) -> Vec<ZoneWrapper> {
    let cleaned = strip_comments_and_strings(dart_source);
    let bytes = cleaned.as_bytes();
    let len = bytes.len();
    let mut found = Vec::new();
    let mut i = 0usize;

    while i < len {
        if !is_ident_start(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < len && is_ident_continue(bytes[i]) {
            i += 1;
        }
        let ident = &cleaned[start..i];
        match ident {
            "runZonedGuarded" | "runZoned" => {
                if let Some(after_type_args) = skip_optional_type_args(&cleaned, i)
                    && after_type_args < len
                    && bytes[after_type_args] == b'('
                {
                    let wrapper =
                        if ident == "runZonedGuarded" { ZoneWrapper::RunZonedGuarded } else { ZoneWrapper::RunZoned };
                    if !found.contains(&wrapper) {
                        found.push(wrapper);
                    }
                }
            }
            "SentryFlutter"
                if is_sentry_init_with_app_runner(&cleaned, i) && !found.contains(&ZoneWrapper::SentryAppRunner) =>
            {
                found.push(ZoneWrapper::SentryAppRunner);
            }
            _ => {}
        }
    }

    found
}

/// Parses a Dart file to determine the signature of its top-level `main` function.
///
/// Ignores comments, string literals, and nested class/function declarations.
/// Defaults to sync with no arguments when no top-level `main` is found (e.g. a re-export).
pub fn parse_main_signature(dart_source: &str) -> MainSignature {
    let cleaned = strip_comments_and_strings(dart_source);
    let bytes = cleaned.as_bytes();
    let len = bytes.len();

    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut stmt_start = 0usize;
    let mut i = 0usize;

    while i < len {
        let b = bytes[i];
        match b {
            b'{' => {
                brace_depth += 1;
                i += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                    stmt_start = i + 1;
                }
                i += 1;
            }
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            b'[' => {
                bracket_depth += 1;
                i += 1;
            }
            b']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                i += 1;
            }
            b';' => {
                if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                    stmt_start = i + 1;
                }
                i += 1;
            }
            _ if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 && is_ident_start(b) => {
                let start = i;
                while i < len && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                if &cleaned[start..i] == "main" {
                    let after_name = skip_ws(&cleaned, i);
                    if after_name < len && bytes[after_name] == b'(' {
                        let prefix = &cleaned[stmt_start..start];
                        if !is_disqualified_prefix(prefix)
                            && let Some(close_paren) = find_matching_paren(&cleaned, after_name)
                        {
                            let params = cleaned[after_name + 1..close_paren].trim();
                            let after_params = skip_ws(&cleaned, close_paren + 1);
                            let is_async = starts_with_keyword(&cleaned[after_params..], "async")
                                && !cleaned[after_params..].starts_with("async*");
                            let return_type = extract_return_type(prefix);
                            let returns_future = match return_type.as_deref() {
                                Some("void") => false,
                                Some(rt) if is_future_type(rt) => true,
                                Some(_) => false,
                                None => is_async,
                            };
                            return MainSignature { returns_future, takes_args: !params.is_empty() };
                        }
                    }
                }
            }
            _ => i += 1,
        }
    }

    MainSignature::default()
}

/// Renders the contents of `lib/main_tapwing.dart` importing `target_rel` relative to `output_rel`.
pub fn render_entrypoint(target_rel: &Path, output_rel: &Path, sig: MainSignature) -> String {
    render_entrypoint_with_zones(target_rel, output_rel, sig, &[])
}

/// Renders `lib/main_tapwing.dart`, adding a zone-mismatch note when `zone_wrappers` is non-empty.
pub fn render_entrypoint_with_zones(
    target_rel: &Path,
    output_rel: &Path,
    sig: MainSignature,
    zone_wrappers: &[ZoneWrapper],
) -> String {
    let import_uri = relative_import_uri(output_rel, target_rel);

    let (signature, call) = match (sig.returns_future, sig.takes_args) {
        (false, false) => ("void main()", "app.main();"),
        (false, true) => ("void main(List<String> args)", "app.main(args);"),
        (true, false) => ("Future<void> main() async", "await app.main();"),
        (true, true) => ("Future<void> main(List<String> args) async", "await app.main(args);"),
    };

    let zone_note = if zone_wrappers.is_empty() {
        String::new()
    } else {
        let target_display = target_rel.to_string_lossy().replace('\\', "/");
        let labels = zone_wrappers.iter().map(|w| w.label()).collect::<Vec<_>>().join(", ");
        format!(
            "// NOTE: `{target_display}` uses `{labels}`.\n\
             // Calling `TapwingBinding.ensureInitialized()` before `app.main()` can trigger\n\
             // Flutter's \"Zone mismatch.\" warning in debug mode because `runApp()` runs in\n\
             // the child zone. To avoid this, pass `TapwingBinding.ensureInitialized` into\n\
             // your zone / `appRunner` callback before `runApp()` (see docs/setup.md).\n"
        )
    };

    format!(
        "// Generated by `tapwing init`. Production code stays untouched;\n\
         // `tapwing_driver` is a dev dependency.\n\
         // ignore_for_file: depend_on_referenced_packages\n\
         import 'package:tapwing_driver/tapwing_driver.dart';\n\
         \n\
         import '{import_uri}' as app;\n\
         \n\
         {zone_note}{signature} {{\n\
         \x20\x20TapwingBinding.ensureInitialized();\n\
         \x20\x20{call}\n\
         }}\n"
    )
}

/// Computes the forward-slash relative Dart import URI from `output_rel`'s directory to `target_rel`.
pub(crate) fn relative_import_uri(output_rel: &Path, target_rel: &Path) -> String {
    let from_dir = output_rel.parent().unwrap_or_else(|| Path::new(""));
    let from_parts = normal_components(from_dir);
    let to_parts = normal_components(target_rel);

    let common = from_parts.iter().zip(to_parts.iter()).take_while(|(a, b)| a == b).count();
    let mut segments: Vec<&str> = vec![".."; from_parts.len() - common];
    for part in &to_parts[common..] {
        segments.push(part);
    }
    segments.join("/")
}

fn normal_components(path: &Path) -> Vec<String> {
    let mut parts = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
            Component::ParentDir => {
                if parts.last().is_some_and(|p| p != "..") {
                    parts.pop();
                } else {
                    parts.push("..".to_string());
                }
            }
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    parts
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_ident_continue(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit()
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    let bytes = s.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Skips whitespace and an optional `<...>` generic type-argument list.
/// Returns `None` if a `<` is not a balanced type-argument list (e.g. a comparison).
fn skip_optional_type_args(s: &str, i: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let after_ws = skip_ws(s, i);
    if after_ws >= bytes.len() || bytes[after_ws] != b'<' {
        return Some(after_ws);
    }
    let mut depth = 0usize;
    let mut j = after_ws;
    while j < bytes.len() {
        match bytes[j] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(skip_ws(s, j + 1));
                }
            }
            b';' | b'{' | b'}' | b'(' | b')' | b'=' => return None,
            _ => {}
        }
        j += 1;
    }
    None
}

/// Returns `true` when `SentryFlutter` at `after_ident` is followed by `.init(..., appRunner: ...)`.
fn is_sentry_init_with_app_runner(s: &str, after_ident: usize) -> bool {
    let bytes = s.as_bytes();
    let dot = skip_ws(s, after_ident);
    if dot >= bytes.len() || bytes[dot] != b'.' {
        return false;
    }
    let method = skip_ws(s, dot + 1);
    if !starts_with_keyword(&s[method..], "init") {
        return false;
    }
    let open = skip_ws(s, method + "init".len());
    if open >= bytes.len() || bytes[open] != b'(' {
        return false;
    }
    let Some(close) = find_matching_paren(s, open) else {
        return false;
    };
    has_named_arg(&s[open + 1..close], "appRunner")
}

/// Returns `true` when `args` contains a top-level named argument `<name>:` at depth 0.
fn has_named_arg(args: &str, name: &str) -> bool {
    let bytes = args.as_bytes();
    let len = bytes.len();
    let mut paren_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut i = 0usize;

    while i < len {
        match bytes[i] {
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            b'{' => {
                brace_depth += 1;
                i += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                i += 1;
            }
            b'[' => {
                bracket_depth += 1;
                i += 1;
            }
            b']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                i += 1;
            }
            _ if paren_depth == 0 && brace_depth == 0 && bracket_depth == 0 && is_ident_start(bytes[i]) => {
                let start = i;
                while i < len && is_ident_continue(bytes[i]) {
                    i += 1;
                }
                if &args[start..i] == name {
                    let prev_non_ws = args[..start].trim_end();
                    if !prev_non_ws.ends_with('.') {
                        let after = skip_ws(args, i);
                        if after < len && bytes[after] == b':' {
                            return true;
                        }
                    }
                }
            }
            _ => i += 1,
        }
    }
    false
}

fn find_matching_paren(s: &str, open_idx: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    for (i, &b) in bytes.iter().enumerate().skip(open_idx) {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn starts_with_keyword(s: &str, kw: &str) -> bool {
    if !s.starts_with(kw) {
        return false;
    }
    let rest = &s.as_bytes()[kw.len()..];
    rest.first().is_none_or(|&b| !is_ident_continue(b))
}

/// Rejects tokens immediately preceding `main` that indicate it is not a top-level function declaration.
fn is_disqualified_prefix(prefix: &str) -> bool {
    let trimmed = prefix.trim_end();
    if trimmed.ends_with('.') {
        return true;
    }
    let last_word = trimmed.rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$')).next().unwrap_or("");
    matches!(
        last_word,
        "get" | "set" | "class" | "enum" | "mixin" | "typedef" | "extension" | "import" | "export" | "part" | "library"
    )
}

/// Extracts the return type token sequence before `main`, stripping leading Dart `@metadata` annotations.
fn extract_return_type(prefix: &str) -> Option<String> {
    let s = strip_leading_annotations(prefix).trim();
    if s.is_empty() {
        return None;
    }
    // Strip `external` modifier if present.
    let s = s.strip_prefix("external").filter(|rest| rest.starts_with(char::is_whitespace)).map_or(s, str::trim);
    if s.is_empty() { None } else { Some(s.to_string()) }
}

fn strip_leading_annotations(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut i = 0;
    loop {
        i = skip_ws(s, i);
        if i >= bytes.len() || bytes[i] != b'@' {
            return &s[i..];
        }
        i += 1;
        // Annotation name (possibly prefixed, e.g. `foo.Bar`).
        while i < bytes.len() && (is_ident_continue(bytes[i]) || bytes[i] == b'.') {
            i += 1;
        }
        // Optional type args `<...>` before constructor arguments.
        let after_name = skip_ws(s, i);
        if after_name < bytes.len() && bytes[after_name] == b'<' {
            let mut depth = 0usize;
            let mut j = after_name;
            while j < bytes.len() {
                match bytes[j] {
                    b'<' => depth += 1,
                    b'>' => {
                        depth -= 1;
                        if depth == 0 {
                            j += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            i = j;
        }
        // Optional argument list `(...)`.
        let after_type = skip_ws(s, i);
        if after_type < bytes.len() && bytes[after_type] == b'(' {
            if let Some(close) = find_matching_paren(s, after_type) {
                i = close + 1;
            } else {
                return "";
            }
        }
    }
}

fn is_future_type(rt: &str) -> bool {
    // Handle `Future<void>`, `Future`, `FutureOr<void>`, `async.Future<void>`, `Future<void>?`.
    let base = rt.split('<').next().unwrap_or(rt).trim().trim_end_matches('?');
    let ident = base.rsplit('.').next().unwrap_or(base);
    ident == "Future" || ident == "FutureOr"
}

/// Replaces Dart comments and string literal contents with spaces (preserving newlines)
/// so brace/paren counting and identifier scanning never trigger on comments or strings.
fn strip_comments_and_strings(src: &str) -> String {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0usize;

    while i < len {
        // Line comment `// ...`
        if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            out.push_str("  ");
            i += 2;
            while i < len && bytes[i] != b'\n' {
                out.push(' ');
                i += 1;
            }
            continue;
        }

        // Block comment `/* ... */` (Dart supports nested block comments).
        if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let mut depth = 1usize;
            out.push_str("  ");
            i += 2;
            while i < len && depth > 0 {
                if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if bytes[i] == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                } else {
                    out.push(if bytes[i] == b'\n' { '\n' } else { ' ' });
                    i += 1;
                }
            }
            continue;
        }

        // Raw string `r'...'`, `r"..."`, `r'''...'''`, `r"""..."""`.
        let is_raw = bytes[i] == b'r'
            && i + 1 < len
            && (bytes[i + 1] == b'\'' || bytes[i + 1] == b'"')
            && (i == 0 || !is_ident_continue(bytes[i - 1]));

        if is_raw || bytes[i] == b'\'' || bytes[i] == b'"' {
            if is_raw {
                out.push(' ');
                i += 1;
            }
            let quote = bytes[i];
            let is_triple = i + 2 < len && bytes[i + 1] == quote && bytes[i + 2] == quote;
            let delim_len = if is_triple { 3 } else { 1 };
            for _ in 0..delim_len {
                out.push(' ');
            }
            i += delim_len;

            while i < len {
                if !is_raw && bytes[i] == b'\\' && i + 1 < len {
                    out.push(' ');
                    out.push(if bytes[i + 1] == b'\n' { '\n' } else { ' ' });
                    i += 2;
                    continue;
                }
                if is_triple && i + 2 < len && bytes[i] == quote && bytes[i + 1] == quote && bytes[i + 2] == quote {
                    out.push_str("   ");
                    i += 3;
                    break;
                }
                if !is_triple && bytes[i] == quote {
                    out.push(' ');
                    i += 1;
                    break;
                }
                out.push(if bytes[i] == b'\n' { '\n' } else { ' ' });
                i += 1;
            }
            continue;
        }

        // Copy ordinary UTF-8 character verbatim.
        if let Some(ch) = src[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sync_no_args() {
        let src = "import 'package:flutter/material.dart';\nvoid main() {\n  runApp(const MyApp());\n}\n";
        assert_eq!(parse_main_signature(src), MainSignature { returns_future: false, takes_args: false });
    }

    #[test]
    fn parses_future_void_async_no_args() {
        let src = "Future<void> main() async {\n  await init();\n  runApp(const MyApp());\n}\n";
        assert_eq!(parse_main_signature(src), MainSignature { returns_future: true, takes_args: false });
    }

    #[test]
    fn parses_future_void_async_with_args() {
        let src = "Future<void> main(List<String> args) async {\n  runApp(MyApp(args));\n}\n";
        assert_eq!(parse_main_signature(src), MainSignature { returns_future: true, takes_args: true });
    }

    #[test]
    fn parses_sync_with_args() {
        let src = "void main(List<String> arguments) {\n  runApp(MyApp(arguments));\n}\n";
        assert_eq!(parse_main_signature(src), MainSignature { returns_future: false, takes_args: true });
    }

    #[test]
    fn void_async_main_does_not_await() {
        // Awaiting a `void`-typed function is a Dart compile error.
        let src = "void main() async {\n  await init();\n  runApp(const MyApp());\n}\n";
        assert_eq!(parse_main_signature(src), MainSignature { returns_future: false, takes_args: false });
    }

    #[test]
    fn untyped_async_main_awaits() {
        let src = "main(List<String> args) async {\n  await init(args);\n}\n";
        assert_eq!(parse_main_signature(src), MainSignature { returns_future: true, takes_args: true });
    }

    #[test]
    fn ignores_comments_strings_and_class_methods() {
        let src = r#"
/// Future<void> main(List<String> args) async {}
// Future<void> main(List<String> args) async {}
/* Future<void> main(List<String> args) async {} */
const example = 'Future<void> main(List<String> args) async {}';

class Runner {
  Future<void> main(List<String> args) async {}
}

@pragma('vm:entry-point')
Future<void> main(/* ignored comment */) => bootstrap();
"#;
        assert_eq!(parse_main_signature(src), MainSignature { returns_future: true, takes_args: false });
    }

    #[test]
    fn renders_relative_imports_and_forwarding() {
        let rendered = render_entrypoint(
            Path::new("lib/flavors/main_dev.dart"),
            Path::new("lib/main_tapwing.dart"),
            MainSignature { returns_future: true, takes_args: true },
        );
        assert!(rendered.contains("import 'package:tapwing_driver/tapwing_driver.dart';"));
        assert!(rendered.contains("import 'flavors/main_dev.dart' as app;"));
        assert!(rendered.contains("Future<void> main(List<String> args) async {"));
        assert!(rendered.contains("  TapwingBinding.ensureInitialized();\n  await app.main(args);"));
    }

    #[test]
    fn renders_parent_relative_import_when_output_is_nested() {
        let rendered = render_entrypoint(
            Path::new("lib/main.dart"),
            Path::new("lib/entrypoints/main_tapwing.dart"),
            MainSignature { returns_future: false, takes_args: false },
        );
        assert!(rendered.contains("import '../main.dart' as app;"));
    }

    #[test]
    fn detects_run_zoned_guarded_and_run_zoned() {
        let guarded = r#"
import 'dart:async';
void main() {
  runZonedGuarded<void>(
    () => runApp(const MyApp()),
    (error, stack) {},
  );
}
"#;
        assert_eq!(detect_zone_wrappers(guarded), vec![ZoneWrapper::RunZonedGuarded]);

        let zoned = r#"
import 'dart:async' as async;
void main() {
  async.runZoned(() => runApp(const MyApp()));
}
"#;
        assert_eq!(detect_zone_wrappers(zoned), vec![ZoneWrapper::RunZoned]);
    }

    #[test]
    fn detects_sentry_flutter_init_only_when_app_runner_is_present() {
        let with_runner = r#"
Future<void> main() async {
  await SentryFlutter.init(
    (options) {
      options.dsn = 'https://example@sentry.io/1';
    },
    appRunner: () => runApp(const MyApp()),
  );
}
"#;
        assert_eq!(detect_zone_wrappers(with_runner), vec![ZoneWrapper::SentryAppRunner]);

        let without_runner = r#"
Future<void> main() async {
  await SentryFlutter.init((options) {
    options.dsn = 'https://example@sentry.io/1';
  });
  runApp(const MyApp());
}
"#;
        assert_eq!(detect_zone_wrappers(without_runner), Vec::<ZoneWrapper>::new());
    }

    #[test]
    fn zone_detection_ignores_comments_strings_and_similar_identifiers() {
        let src = r#"
// runZonedGuarded(() {}, (e, s) {});
/*
  SentryFlutter.init((o) {}, appRunner: () => runApp(const App()));
*/
const note = 'runZoned(() => runApp(const App()))';
const rawNote = r"runZonedGuarded(() {}, (e, s) {})";

void main() {
  const runZonedCustom = true;
  if (runZonedCustom) {
    SentryFlutter.init(
      (options) {
        // appRunner: () {}
        options.dsn = 'appRunner: fake';
      },
    );
    runApp(const MyApp());
  }
}
"#;
        assert_eq!(detect_zone_wrappers(src), Vec::<ZoneWrapper>::new());
    }

    #[test]
    fn renders_zone_note_only_when_wrappers_present() {
        let plain = render_entrypoint_with_zones(
            Path::new("lib/main.dart"),
            Path::new("lib/main_tapwing.dart"),
            MainSignature::default(),
            &[],
        );
        assert!(!plain.contains("Zone mismatch"));

        let with_zone = render_entrypoint_with_zones(
            Path::new("lib/main.dart"),
            Path::new("lib/main_tapwing.dart"),
            MainSignature::default(),
            &[ZoneWrapper::RunZonedGuarded],
        );
        assert!(with_zone.contains("// NOTE: `lib/main.dart` uses `runZonedGuarded`."));
        assert!(with_zone.contains("Zone mismatch."));
        assert!(with_zone.contains("void main() {\n  TapwingBinding.ensureInitialized();\n  app.main();\n}\n"));
    }
}
