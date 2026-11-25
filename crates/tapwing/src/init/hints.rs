//! Post-init next steps and Android `productFlavors` detection.

use std::path::Path;

use super::entrypoint::ZoneWrapper;

/// Inspects `android/app/build.gradle.kts` and `android/app/build.gradle` for `productFlavors`.
///
/// Returns `None` when no Gradle file defines `productFlavors`, or `Some(names)` when a
/// `productFlavors { ... }` block is found (`names` may be empty if dynamically generated).
pub(crate) fn detect_android_flavors(app_dir: &Path) -> Option<Vec<String>> {
    for rel in ["android/app/build.gradle.kts", "android/app/build.gradle"] {
        let path = app_dir.join(rel);
        if let Ok(src) = std::fs::read_to_string(&path)
            && let Some(flavors) = parse_android_flavors(&src)
        {
            return Some(flavors);
        }
    }
    None
}

/// Parses `productFlavors { ... }` blocks from Groovy or Kotlin DSL Gradle build scripts.
pub(crate) fn parse_android_flavors(gradle_src: &str) -> Option<Vec<String>> {
    let cleaned = strip_gradle_comments(gradle_src);
    let block = extract_block_after_keyword(&cleaned, "productFlavors")?;
    let trimmed = block.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut flavors = Vec::new();
    let mut depth = 0usize;
    let bytes = block.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            _ if depth == 0 && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_') => {
                let start = i;
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = &block[start..i];
                let after = skip_ws(block, i);

                // Kotlin DSL: `create("dev") { ... }`, `register("prod") { ... }`, `named("stg") { ... }`
                if matches!(word, "create" | "register" | "named")
                    && after < len
                    && bytes[after] == b'('
                    && let Some((name, close_idx)) = extract_quoted_arg(block, after)
                {
                    push_unique(&mut flavors, name);
                    i = close_idx;
                    continue;
                }

                // Groovy DSL: `dev { ... }`
                if after < len && bytes[after] == b'{' && !is_gradle_dsl_keyword(word) {
                    push_unique(&mut flavors, word.to_string());
                }
            }
            _ => i += 1,
        }
    }

    Some(flavors)
}

/// Formats actionable next steps for the user after `tapwing init`.
pub(crate) fn format_next_steps(
    app_display: Option<&str>,
    output_rel: &Path,
    ran_pub_get: bool,
    android_flavors: Option<&[String]>,
) -> String {
    let output_str = output_rel.to_string_lossy().replace('\\', "/");
    let mut out = String::from("Next steps:\n");

    if let Some(dir) = app_display {
        out.push_str(&format!("  cd {dir}\n"));
    }

    if !ran_pub_get {
        out.push_str("  flutter pub get\n");
    }

    let (flavor_flag, apk_name, flavor_note) = match android_flavors {
        Some(flavors) if !flavors.is_empty() => {
            let sample = &flavors[0];
            let note = format!("  # Android productFlavors detected: {}", flavors.join(", "));
            (format!(" --flavor {sample}"), format!("app-{sample}-profile.apk"), Some(note))
        }
        Some(_) => (
            " --flavor <flavor>".to_string(),
            "app-<flavor>-profile.apk".to_string(),
            Some("  # Android productFlavors detected in android/app/build.gradle*".to_string()),
        ),
        None => (String::new(), "app-profile.apk".to_string(), None),
    };

    out.push('\n');
    if let Some(note) = flavor_note {
        out.push_str(&note);
        out.push('\n');
    }
    out.push_str("  # Android (profile build):\n");
    out.push_str(&format!("  flutter build apk --profile{flavor_flag} -t {output_str}\n"));
    out.push_str(&format!("  adb install -r build/app/outputs/flutter-apk/{apk_name}\n\n"));

    out.push_str("  # iOS Simulator (debug build — simulator has no profile mode):\n");
    out.push_str(&format!("  flutter build ios --simulator --debug{flavor_flag} -t {output_str}\n"));
    out.push_str("  xcrun simctl install booted build/ios/iphonesimulator/Runner.app\n\n");

    out.push_str("  # Run Maestro flows:\n");
    out.push_str("  tapwing test .maestro\n");

    out
}

/// Formats an actionable warning when the target Dart entrypoint creates a zone.
pub(crate) fn format_zone_warning(
    app_display: Option<&str>,
    target_rel: &Path,
    output_rel: &Path,
    wrappers: &[ZoneWrapper],
) -> Option<String> {
    if wrappers.is_empty() {
        return None;
    }
    let target_str = prefixed_rel(app_display, target_rel);
    let output_str = prefixed_rel(app_display, output_rel);
    let labels = wrappers.iter().map(|w| w.label()).collect::<Vec<_>>().join(", ");

    let mut out = format!(
        "⚠ Zone wrapper detected in {target_str} ({labels}):\n\
         \x20 Calling `TapwingBinding.ensureInitialized()` before `app.main()` initializes the\n\
         \x20 Flutter binding in the root zone while `runApp()` runs inside a child zone.\n\
         \x20 In debug mode Flutter reports a \"Zone mismatch.\" warning (surfaced by Tapwing\n\
         \x20 as a non-blocking `setupWarning`; if `BindingBase.debugZoneErrorsAreFatal = true`,\n\
         \x20 startup will throw).\n\n\
         \x20 To initialize Tapwing inside the same zone without changing production behavior,\n\
         \x20 extract a shared `bootstrap()` helper in {target_str} and pass\n\
         \x20 `TapwingBinding.ensureInitialized` from {output_str}:\n\n"
    );

    if wrappers.contains(&ZoneWrapper::SentryAppRunner) {
        out.push_str(
            "    // In your target entrypoint:\n\
             \x20\x20\x20\x20Future<void> bootstrap({void Function()? ensureBinding}) async {\n\
             \x20\x20\x20\x20\x20\x20await SentryFlutter.init(\n\
             \x20\x20\x20\x20\x20\x20\x20\x20(options) => options.dsn = '...',\n\
             \x20\x20\x20\x20\x20\x20\x20\x20appRunner: () {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(ensureBinding ?? WidgetsFlutterBinding.ensureInitialized)();\n\
             \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20runApp(const MyApp());\n\
             \x20\x20\x20\x20\x20\x20\x20\x20},\n\
             \x20\x20\x20\x20\x20\x20);\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20Future<void> main() => bootstrap();\n\n",
        );
    } else {
        out.push_str(
            "    // In your target entrypoint:\n\
             \x20\x20\x20\x20Future<void> bootstrap({void Function()? ensureBinding}) async {\n\
             \x20\x20\x20\x20\x20\x20await runZonedGuarded(() async {\n\
             \x20\x20\x20\x20\x20\x20\x20\x20(ensureBinding ?? WidgetsFlutterBinding.ensureInitialized)();\n\
             \x20\x20\x20\x20\x20\x20\x20\x20runApp(const MyApp());\n\
             \x20\x20\x20\x20\x20\x20}, (error, stack) { /* handle error */ });\n\
             \x20\x20\x20\x20}\n\
             \x20\x20\x20\x20Future<void> main() => bootstrap();\n\n",
        );
    }

    out.push_str(&format!(
        "    // In {output_str}:\n\
         \x20\x20\x20\x20Future<void> main() => app.bootstrap(\n\
         \x20\x20\x20\x20\x20\x20ensureBinding: TapwingBinding.ensureInitialized,\n\
         \x20\x20\x20\x20);\n\n\
         \x20 See docs/setup.md#zones-and-error-wrappers for details.\n"
    ));

    Some(out)
}

fn prefixed_rel(app_display: Option<&str>, rel: &Path) -> String {
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    match app_display {
        Some(dir) => format!("{dir}/{rel_str}"),
        None => rel_str,
    }
}

fn push_unique(list: &mut Vec<String>, item: String) {
    if !item.is_empty() && !list.contains(&item) {
        list.push(item);
    }
}

fn is_gradle_dsl_keyword(word: &str) -> bool {
    matches!(word, "all" | "configureEach" | "matching" | "named" | "create" | "register" | "forEach" | "if" | "for")
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    let bytes = s.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn extract_block_after_keyword<'a>(src: &'a str, keyword: &str) -> Option<&'a str> {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            if &src[start..i] == keyword {
                let open = skip_ws(src, i);
                if open < len && bytes[open] == b'{' {
                    let mut depth = 0usize;
                    for (j, &b) in bytes.iter().enumerate().skip(open) {
                        match b {
                            b'{' => depth += 1,
                            b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    return Some(&src[open + 1..j]);
                                }
                            }
                            _ => {}
                        }
                    }
                    return None;
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn extract_quoted_arg(s: &str, open_paren: usize) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    let q_idx = skip_ws(s, open_paren + 1);
    if q_idx >= bytes.len() || (bytes[q_idx] != b'"' && bytes[q_idx] != b'\'') {
        return None;
    }
    let quote = bytes[q_idx];
    let mut end = q_idx + 1;
    while end < bytes.len() && bytes[end] != quote {
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    let value = s[q_idx + 1..end].to_string();
    let close = skip_ws(s, end + 1);
    if close < bytes.len() && bytes[close] == b')' { Some((value, close + 1)) } else { None }
}

fn strip_gradle_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(len);
            continue;
        }
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
    fn parses_groovy_product_flavors() {
        let gradle = r#"
android {
    flavorDimensions "env"
    // productFlavors { commentedOut {} }
    productFlavors {
        dev {
            dimension "env"
            applicationIdSuffix ".dev"
        }
        prod {
            dimension "env"
        }
    }
}
"#;
        assert_eq!(parse_android_flavors(gradle), Some(vec!["dev".to_string(), "prod".to_string()]));
    }

    #[test]
    fn parses_kotlin_dsl_product_flavors() {
        let gradle = r#"
android {
    flavorDimensions += "env"
    productFlavors {
        create("staging") {
            dimension = "env"
        }
        register("production") {
            dimension = "env"
        }
    }
}
"#;
        assert_eq!(parse_android_flavors(gradle), Some(vec!["staging".to_string(), "production".to_string()]));
    }

    #[test]
    fn returns_none_when_no_product_flavors() {
        let gradle = "android {\n    defaultConfig {\n        applicationId = \"com.example.app\"\n    }\n}\n";
        assert_eq!(parse_android_flavors(gradle), None);
    }

    #[test]
    fn next_steps_includes_flavor_hint_when_detected() {
        let steps = format_next_steps(
            Some("apps/my_app"),
            Path::new("lib/main_tapwing.dart"),
            false,
            Some(&["dev".to_string(), "prod".to_string()]),
        );
        assert!(steps.contains("cd apps/my_app"));
        assert!(steps.contains("flutter pub get"));
        assert!(steps.contains("--flavor dev -t lib/main_tapwing.dart"));
        assert!(steps.contains("detected: dev, prod"));
        assert!(steps.contains("tapwing test .maestro"));
    }
}
