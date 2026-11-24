//! Text-preserving `pubspec.yaml` editor for adding or updating `tapwing_driver`.
//!
//! Never round-trips the file through a YAML serializer: comments, key ordering,
//! blank lines, indentation, and line endings are preserved verbatim.

use anyhow::{Context, Result, anyhow, bail};
use std::path::{Component, Path, PathBuf};
use yaml_rust2::{Yaml, YamlLoader};

/// Dependency source written under `dev_dependencies.tapwing_driver`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverSource {
    Git { url: String, path: String, git_ref: String },
    Path(String),
}

/// Result of planning an edit to `pubspec.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PubspecEdit {
    /// `pubspec.yaml` needs to be written with the returned contents.
    Updated(String),
    /// `tapwing_driver` is already present and `--force` was not passed.
    AlreadyPresent,
    /// `--force` was passed, and the existing entry already matches identically.
    Unchanged,
}

/// Edits `pubspec.yaml` text to add (or with `force`, replace) `tapwing_driver` under `dev_dependencies`.
pub fn edit_pubspec(content: &str, source: &DriverSource, force: bool) -> Result<PubspecEdit> {
    let docs = YamlLoader::load_from_str(content).context("failed to parse pubspec.yaml")?;
    let root =
        docs.first().and_then(Yaml::as_hash).ok_or_else(|| anyhow!("pubspec.yaml root must be a YAML mapping"))?;

    let in_deps = has_map_key(root, "dependencies", "tapwing_driver");
    let in_dev_deps = has_map_key(root, "dev_dependencies", "tapwing_driver");

    if in_deps && !in_dev_deps {
        if force {
            bail!(
                "tapwing_driver is currently listed under `dependencies` instead of `dev_dependencies`; \
                 move or remove it from `dependencies` first"
            );
        }
        return Ok(PubspecEdit::AlreadyPresent);
    }

    if in_dev_deps && !force {
        return Ok(PubspecEdit::AlreadyPresent);
    }

    let eol = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = split_preserving_lines(content);

    if in_dev_deps {
        replace_existing_entry(&mut lines, source)?;
    } else if let Some(dev_idx) = find_top_level_key(&lines, "dev_dependencies") {
        insert_into_dev_dependencies(&mut lines, dev_idx, source)?;
    } else {
        insert_new_dev_dependencies(&mut lines, source);
    }

    let mut updated = lines.join(eol);
    if !updated.ends_with(eol) {
        updated.push_str(eol);
    }

    validate_edited_pubspec(&updated)?;

    if updated == content { Ok(PubspecEdit::Unchanged) } else { Ok(PubspecEdit::Updated(updated)) }
}

/// Computes a portable forward-slash relative path from `app_dir` to `driver_path`.
pub(crate) fn compute_relative_driver_path(app_dir: &Path, driver_path: &Path) -> Result<String> {
    let abs_driver = if driver_path.is_absolute() {
        driver_path.to_path_buf()
    } else {
        std::env::current_dir().context("failed to read current working directory")?.join(driver_path)
    };
    let abs_app = if app_dir.is_absolute() {
        app_dir.to_path_buf()
    } else {
        std::env::current_dir().context("failed to read current working directory")?.join(app_dir)
    };

    // Canonicalize both when both exist so macOS `/var` -> `/private/var` symlinks don't skew relative depth.
    let (norm_app, norm_driver) = match (std::fs::canonicalize(&abs_app), std::fs::canonicalize(&abs_driver)) {
        (Ok(a), Ok(d)) => (a, d),
        _ => (lexical_normalize(&abs_app), lexical_normalize(&abs_driver)),
    };

    let rel = diff_paths(&norm_app, &norm_driver);
    let s = rel.to_string_lossy().replace('\\', "/");
    Ok(if s.is_empty() { ".".to_string() } else { s })
}

fn has_map_key(root: &yaml_rust2::yaml::Hash, section: &str, key: &str) -> bool {
    root.get(&Yaml::String(section.to_string()))
        .and_then(Yaml::as_hash)
        .is_some_and(|m| m.contains_key(&Yaml::String(key.to_string())))
}

fn split_preserving_lines(content: &str) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    let normalized = content.replace("\r\n", "\n");
    let trimmed = normalized.strip_suffix('\n').unwrap_or(&normalized);
    trimmed.split('\n').map(String::from).collect()
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|&b| b == b' ').count()
}

fn is_blank_or_comment(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

/// Returns true when `line` starts the key `key:` at column `expected_indent`.
fn is_mapping_key(line: &str, expected_indent: usize, key: &str) -> bool {
    if leading_spaces(line) != expected_indent {
        return false;
    }
    let rest = &line[expected_indent..];
    let Some(after_key) = rest.strip_prefix(key) else {
        return false;
    };
    let after_key = after_key.trim_start_matches(' ');
    let Some(after_colon) = after_key.strip_prefix(':') else {
        return false;
    };
    after_colon.is_empty()
        || after_colon.starts_with(' ')
        || after_colon.starts_with('\t')
        || after_colon.starts_with('#')
}

fn find_top_level_key(lines: &[String], key: &str) -> Option<usize> {
    lines.iter().position(|line| is_mapping_key(line, 0, key))
}

/// Returns the exclusive end index of the top-level YAML section starting at `section_start`.
fn top_level_section_end(lines: &[String], section_start: usize) -> usize {
    let mut i = section_start + 1;
    while i < lines.len() {
        let line = &lines[i];
        if !is_blank_or_comment(line) && leading_spaces(line) == 0 {
            break;
        }
        i += 1;
    }
    i
}

/// Finds the last non-blank, non-comment indented content line inside `[section_start + 1 .. section_end]`.
fn last_content_line_in_section(lines: &[String], section_start: usize, section_end: usize) -> Option<usize> {
    ((section_start + 1)..section_end)
        .rev()
        .find(|&idx| !is_blank_or_comment(&lines[idx]) && leading_spaces(&lines[idx]) > 0)
}

/// Detects the direct child indentation of a top-level mapping section (defaulting to 2 spaces).
fn detect_child_indent(lines: &[String], section_start: usize, section_end: usize) -> usize {
    ((section_start + 1)..section_end)
        .filter(|&idx| !is_blank_or_comment(&lines[idx]))
        .map(|idx| leading_spaces(&lines[idx]))
        .filter(|&indent| indent > 0)
        .min()
        .unwrap_or(2)
}

fn insert_into_dev_dependencies(lines: &mut Vec<String>, dev_idx: usize, source: &DriverSource) -> Result<()> {
    let section_end = top_level_section_end(lines, dev_idx);
    let child_indent = detect_child_indent(lines, dev_idx, section_end);

    // Handle `dev_dependencies: {}` (or `dev_dependencies: {} # comment`) by clearing the empty flow map.
    if let Some(rewritten) = rewrite_empty_flow_map(&lines[dev_idx], "dev_dependencies") {
        lines[dev_idx] = rewritten;
    } else if has_non_empty_inline_value(&lines[dev_idx], "dev_dependencies") {
        bail!(
            "unsupported inline flow mapping on `dev_dependencies:` in pubspec.yaml; \
             convert `dev_dependencies` to block YAML syntax and re-run"
        );
    }

    let insert_at = last_content_line_in_section(lines, dev_idx, section_end).map_or(dev_idx + 1, |last| last + 1);
    let block = format_driver_entry(source, child_indent);
    lines.splice(insert_at..insert_at, block);
    Ok(())
}

fn insert_new_dev_dependencies(lines: &mut Vec<String>, source: &DriverSource) {
    let mut block = Vec::with_capacity(7);
    block.push(String::new());
    block.push("dev_dependencies:".to_string());
    block.extend(format_driver_entry(source, 2));

    if let Some(deps_idx) = find_top_level_key(lines, "dependencies") {
        let deps_end = top_level_section_end(lines, deps_idx);
        let insert_at = last_content_line_in_section(lines, deps_idx, deps_end).map_or(deps_idx + 1, |last| last + 1);
        lines.splice(insert_at..insert_at, block);
    } else {
        // Trim trailing empty lines before appending so we always separate by exactly one blank line.
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        if lines.is_empty() {
            // Remove leading blank line if file was somehow empty.
            block.remove(0);
        }
        lines.extend(block);
    }
}

fn replace_existing_entry(lines: &mut Vec<String>, source: &DriverSource) -> Result<()> {
    let dev_idx = find_top_level_key(lines, "dev_dependencies")
        .ok_or_else(|| anyhow!("could not locate `dev_dependencies:` in pubspec.yaml"))?;
    let section_end = top_level_section_end(lines, dev_idx);
    let child_indent = detect_child_indent(lines, dev_idx, section_end);

    let entry_start = ((dev_idx + 1)..section_end)
        .find(|&idx| is_mapping_key(&lines[idx], child_indent, "tapwing_driver"))
        .ok_or_else(|| {
            anyhow!(
                "could not locate block `tapwing_driver:` entry under `dev_dependencies:` in pubspec.yaml \
                 (inline flow mappings are not supported for --force)"
            )
        })?;

    // Find the end of the `tapwing_driver:` entry (all subsequent lines strictly more indented),
    // stopping before any trailing blank lines or comments at `<= child_indent`.
    let mut scan = entry_start + 1;
    let mut entry_end = entry_start + 1;
    while scan < section_end {
        let line = &lines[scan];
        if line.trim().is_empty() {
            scan += 1;
            continue;
        }
        let indent = leading_spaces(line);
        if indent <= child_indent {
            break;
        }
        entry_end = scan + 1;
        scan += 1;
    }

    let replacement = format_driver_entry(source, child_indent);
    lines.splice(entry_start..entry_end, replacement);
    Ok(())
}

fn rewrite_empty_flow_map(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start_matches(' ').strip_prefix(':')?;
    let (val_part, comment_part) = split_inline_comment(rest);
    if val_part.trim() == "{}" || val_part.trim() == "null" || val_part.trim() == "~" {
        let mut out = format!("{key}:");
        if let Some(comment) = comment_part {
            out.push(' ');
            out.push_str(comment);
        }
        Some(out)
    } else {
        None
    }
}

fn has_non_empty_inline_value(line: &str, key: &str) -> bool {
    let Some(rest) = line.strip_prefix(key).and_then(|s| s.trim_start_matches(' ').strip_prefix(':')) else {
        return false;
    };
    let (val_part, _) = split_inline_comment(rest);
    !val_part.trim().is_empty()
}

fn split_inline_comment(s: &str) -> (&str, Option<&str>) {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'#' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
            return (&s[..i], Some(s[i..].trim_end()));
        }
    }
    (s, None)
}

fn format_driver_entry(source: &DriverSource, indent: usize) -> Vec<String> {
    let step = indent.max(2);
    let pad1 = " ".repeat(indent);
    let pad2 = " ".repeat(indent + step);
    let pad3 = " ".repeat(indent + step * 2);

    match source {
        DriverSource::Git { url, path, git_ref } => vec![
            format!("{pad1}tapwing_driver:"),
            format!("{pad2}git:"),
            format!("{pad3}url: {}", yaml_scalar(url)),
            format!("{pad3}path: {}", yaml_scalar(path)),
            format!("{pad3}ref: {}", yaml_scalar(git_ref)),
        ],
        DriverSource::Path(path) => {
            vec![format!("{pad1}tapwing_driver:"), format!("{pad2}path: {}", yaml_scalar(path))]
        }
    }
}

/// Formats a YAML scalar without quotes when it round-trips identically as a plain string,
/// otherwise single-quotes it safely.
fn yaml_scalar(value: &str) -> String {
    let safe_plain = !value.is_empty()
        && value.trim() == value
        && !value.contains('\n')
        && !value.contains('\r')
        && !value.contains('\t')
        && !value.contains('#')
        && !value.contains(": ")
        && !value.ends_with(':')
        && YamlLoader::load_from_str(value)
            .ok()
            .and_then(|docs| docs.into_iter().next())
            .is_some_and(|doc| matches!(doc, Yaml::String(s) if s == value));

    if safe_plain { value.to_string() } else { format!("'{}'", value.replace('\'', "''")) }
}

fn validate_edited_pubspec(updated: &str) -> Result<()> {
    let docs = YamlLoader::load_from_str(updated).context("internal error: edited pubspec.yaml is invalid YAML")?;
    let root = docs.first().and_then(Yaml::as_hash).ok_or_else(|| anyhow!("edited pubspec.yaml is not a mapping"))?;
    if !has_map_key(root, "dev_dependencies", "tapwing_driver") {
        bail!("internal error: edited pubspec.yaml is missing dev_dependencies.tapwing_driver");
    }
    Ok(())
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(comp);
                }
            }
            _ => out.push(comp),
        }
    }
    out
}

fn diff_paths(base: &Path, target: &Path) -> PathBuf {
    let base_comps: Vec<Component<'_>> = base.components().collect();
    let target_comps: Vec<Component<'_>> = target.components().collect();

    let common = base_comps.iter().zip(target_comps.iter()).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..base_comps.len() {
        out.push("..");
    }
    for comp in &target_comps[common..] {
        out.push(comp);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_source() -> DriverSource {
        DriverSource::Git {
            url: "https://github.com/tapwing-dev/tapwing.git".to_string(),
            path: "packages/tapwing_driver".to_string(),
            git_ref: "v0.1.0".to_string(),
        }
    }

    #[test]
    fn inserts_into_existing_dev_dependencies_preserving_comments_and_order() {
        let src = "\
name: demo
# top comment
dependencies:
  flutter:
    sdk: flutter

dev_dependencies:
  flutter_test:
    sdk: flutter
  # keep this comment above flutter_lints
  flutter_lints: ^6.0.0

# Flutter section comment stays attached to flutter:
flutter:
  uses-material-design: true
";
        let PubspecEdit::Updated(out) = edit_pubspec(src, &git_source(), false).unwrap() else {
            panic!("expected Updated");
        };

        let expected = "\
name: demo
# top comment
dependencies:
  flutter:
    sdk: flutter

dev_dependencies:
  flutter_test:
    sdk: flutter
  # keep this comment above flutter_lints
  flutter_lints: ^6.0.0
  tapwing_driver:
    git:
      url: https://github.com/tapwing-dev/tapwing.git
      path: packages/tapwing_driver
      ref: v0.1.0

# Flutter section comment stays attached to flutter:
flutter:
  uses-material-design: true
";
        assert_eq!(out, expected);
    }

    #[test]
    fn creates_dev_dependencies_when_missing_preserving_comments() {
        let src = "\
name: demo
# comment before deps
dependencies:
  flutter:
    sdk: flutter

# flutter section comment
flutter:
  uses-material-design: true
";
        let PubspecEdit::Updated(out) =
            edit_pubspec(src, &DriverSource::Path("../tapwing_driver".into()), false).unwrap()
        else {
            panic!("expected Updated");
        };

        let expected = "\
name: demo
# comment before deps
dependencies:
  flutter:
    sdk: flutter

dev_dependencies:
  tapwing_driver:
    path: ../tapwing_driver

# flutter section comment
flutter:
  uses-material-design: true
";
        assert_eq!(out, expected);
    }

    #[test]
    fn handles_empty_flow_dev_dependencies() {
        let src = "name: demo\ndependencies:\n  flutter:\n    sdk: flutter\ndev_dependencies: {} # empty\n";
        let PubspecEdit::Updated(out) = edit_pubspec(src, &DriverSource::Path("../d".into()), false).unwrap() else {
            panic!("expected Updated");
        };
        assert!(out.contains("dev_dependencies: # empty\n  tapwing_driver:\n    path: ../d\n"));
    }

    #[test]
    fn idempotent_without_force_and_replaces_with_force() {
        let src = "\
name: demo
dependencies:
  flutter:
    sdk: flutter
dev_dependencies:
  flutter_test:
    sdk: flutter
  tapwing_driver:
    git:
      url: https://github.com/tapwing-dev/tapwing.git
      path: packages/tapwing_driver
      ref: v0.0.1
  # keep comment for next dep
  flutter_lints: ^6.0.0
";
        assert_eq!(edit_pubspec(src, &git_source(), false).unwrap(), PubspecEdit::AlreadyPresent);

        let PubspecEdit::Updated(out) =
            edit_pubspec(src, &DriverSource::Path("../../packages/tapwing_driver".into()), true).unwrap()
        else {
            panic!("expected Updated with --force");
        };

        let expected = "\
name: demo
dependencies:
  flutter:
    sdk: flutter
dev_dependencies:
  flutter_test:
    sdk: flutter
  tapwing_driver:
    path: ../../packages/tapwing_driver
  # keep comment for next dep
  flutter_lints: ^6.0.0
";
        assert_eq!(out, expected);
    }

    #[test]
    fn quotes_numeric_looking_git_refs() {
        assert_eq!(yaml_scalar("v0.1.0"), "v0.1.0");
        assert_eq!(
            yaml_scalar("https://github.com/tapwing-dev/tapwing.git"),
            "https://github.com/tapwing-dev/tapwing.git"
        );
        assert_eq!(yaml_scalar("file:///tmp/tapwing"), "file:///tmp/tapwing");
        assert_eq!(yaml_scalar("1234567"), "'1234567'");
        assert_eq!(yaml_scalar("1234e56"), "'1234e56'");
        assert_eq!(yaml_scalar("true"), "'true'");
        assert_eq!(yaml_scalar("../my path/tapwing_driver"), "../my path/tapwing_driver");
    }
}
