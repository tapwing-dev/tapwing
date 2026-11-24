//! Flutter app and Dart pub workspace detection for `tapwing init`.

use anyhow::{Context, Result, anyhow, bail};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use yaml_rust2::{Yaml, YamlLoader};

use super::DEFAULT_TARGET;

/// Resolved Flutter application target for `tapwing init`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedApp {
    /// Directory passed to `tapwing init` (or `.`).
    pub root_dir: PathBuf,
    /// Resolved Flutter app directory containing the target `pubspec.yaml`.
    pub app_dir: PathBuf,
    /// Relative path from `root_dir` to `app_dir` when inside a workspace (or `None` when same dir).
    pub app_rel_display: Option<String>,
    /// Dart package `name` from the app's `pubspec.yaml`.
    pub package_name: String,
    /// Target entrypoint path relative to `app_dir` (e.g. `lib/main.dart`).
    pub target_rel: PathBuf,
    /// Generated Tapwing entrypoint path relative to `app_dir` (e.g. `lib/main_tapwing.dart`).
    pub output_rel: PathBuf,
}

/// Detects the Flutter app to initialize, handling both plain apps and Dart pub workspaces.
pub fn detect_flutter_app(
    project_dir: &Path,
    app_override: Option<&Path>,
    target: &Path,
    output: &Path,
) -> Result<DetectedApp> {
    if !project_dir.exists() {
        bail!("project directory does not exist: {}", project_dir.display());
    }
    if !project_dir.is_dir() {
        bail!("project path is not a directory: {}", project_dir.display());
    }

    let (app_dir, app_rel_display) = resolve_app_dir(project_dir, app_override, target, output)?;
    let pubspec_path = app_dir.join("pubspec.yaml");
    if !pubspec_path.is_file() {
        bail!("no pubspec.yaml found in {} (not a Flutter app)", app_dir.display());
    }

    let pubspec_text =
        std::fs::read_to_string(&pubspec_path).with_context(|| format!("failed to read {}", pubspec_path.display()))?;
    let root = parse_pubspec_mapping(&pubspec_text, &pubspec_path)?;

    if !has_flutter_sdk_dep(&root) {
        bail!(
            "{} does not declare a Flutter SDK dependency (`dependencies.flutter.sdk: flutter`)",
            pubspec_path.display()
        );
    }

    let package_name = root
        .get(&Yaml::String("name".to_string()))
        .and_then(Yaml::as_str)
        .map(String::from)
        .ok_or_else(|| anyhow!("{} is missing a top-level `name:` field", pubspec_path.display()))?;

    let target_rel = normalize_app_relative(&app_dir, target)?;
    let output_rel = normalize_app_relative(&app_dir, output)?;

    if target_rel == output_rel {
        bail!("--output ({}) cannot be the same file as --target ({})", output_rel.display(), target_rel.display());
    }

    let target_abs = app_dir.join(&target_rel);
    if !target_abs.is_file() {
        let found = find_candidate_entrypoints(&app_dir, &output_rel);
        if !found.is_empty() {
            let list = found.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect::<Vec<_>>().join(", ");
            bail!(
                "entrypoint {} not found in {}; found {list} — pass --target <file>",
                target_rel.display(),
                app_dir.display()
            );
        }
        bail!("entrypoint {} not found in {}", target_rel.display(), app_dir.display());
    }

    Ok(DetectedApp {
        root_dir: project_dir.to_path_buf(),
        app_dir,
        app_rel_display,
        package_name,
        target_rel,
        output_rel,
    })
}

fn resolve_app_dir(
    project_dir: &Path,
    app_override: Option<&Path>,
    target: &Path,
    output: &Path,
) -> Result<(PathBuf, Option<String>)> {
    if let Some(app_arg) = app_override {
        let direct = if app_arg.is_absolute() { app_arg.to_path_buf() } else { project_dir.join(app_arg) };
        if direct.is_dir() {
            let rel = display_relative(project_dir, &direct);
            return Ok((direct, rel));
        }

        // Also allow selecting a pub workspace member by package name if unambiguous.
        if let Some(root) = try_read_pubspec_mapping(&project_dir.join("pubspec.yaml"))?
            && let Some(entries) = workspace_entries(&root)
        {
            let name_query = app_arg.to_string_lossy();
            let members = expand_workspace_members(project_dir, &entries);
            let mut matched = Vec::new();
            for member in &members {
                if let Some(m_root) = try_read_pubspec_mapping(&member.join("pubspec.yaml"))?
                    && m_root.get(&Yaml::String("name".to_string())).and_then(Yaml::as_str) == Some(name_query.as_ref())
                {
                    matched.push(member.clone());
                }
            }
            if let [only] = matched.as_slice() {
                let rel = display_relative(project_dir, only);
                return Ok((only.clone(), rel));
            }
        }

        bail!("--app directory does not exist: {}", direct.display());
    }

    let root_pubspec_path = project_dir.join("pubspec.yaml");
    if !root_pubspec_path.is_file() {
        bail!("no pubspec.yaml found in {} (not a Flutter project)", project_dir.display());
    }

    let root_text = std::fs::read_to_string(&root_pubspec_path)
        .with_context(|| format!("failed to read {}", root_pubspec_path.display()))?;
    let root = parse_pubspec_mapping(&root_text, &root_pubspec_path)?;

    if let Some(entries) = workspace_entries(&root) {
        let members = expand_workspace_members(project_dir, &entries);
        let mut candidates: Vec<(String, PathBuf)> = Vec::new();

        // Include the workspace root itself if it is also a Flutter app (common root-app + packages layout).
        if is_flutter_app_dir(project_dir, &root, target, output) {
            candidates.push((".".to_string(), project_dir.to_path_buf()));
        }

        for member in &members {
            if let Some(m_root) = try_read_pubspec_mapping(&member.join("pubspec.yaml"))?
                && is_flutter_app_dir(member, &m_root, target, output)
            {
                let label = display_relative(project_dir, member).unwrap_or_else(|| member.display().to_string());
                candidates.push((label, member.clone()));
            }
        }

        candidates.sort_by(|a, b| a.0.cmp(&b.0));

        match candidates.as_slice() {
            [] => {
                let checked = members
                    .iter()
                    .map(|m| display_relative(project_dir, m).unwrap_or_else(|| m.display().to_string()))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "no Flutter app with an entrypoint found in pub workspace {} (checked: {checked})",
                    project_dir.display()
                );
            }
            [(label, dir)] => {
                let rel = if label == "." { None } else { Some(label.clone()) };
                Ok((dir.clone(), rel))
            }
            many => {
                let list = many.iter().map(|(label, _)| format!("  - {label}")).collect::<Vec<_>>().join("\n");
                bail!(
                    "multiple Flutter apps found in pub workspace {}; pass --app <dir>:\n{list}",
                    project_dir.display()
                );
            }
        }
    } else {
        Ok((project_dir.to_path_buf(), None))
    }
}

fn parse_pubspec_mapping(text: &str, path: &Path) -> Result<yaml_rust2::yaml::Hash> {
    let docs = YamlLoader::load_from_str(text).with_context(|| format!("failed to parse {}", path.display()))?;
    docs.into_iter()
        .next()
        .and_then(|y| y.into_hash())
        .ok_or_else(|| anyhow!("{} root must be a YAML mapping", path.display()))
}

fn try_read_pubspec_mapping(path: &Path) -> Result<Option<yaml_rust2::yaml::Hash>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_pubspec_mapping(&text, path).map(Some)
}

fn has_flutter_sdk_dep(root: &yaml_rust2::yaml::Hash) -> bool {
    root.get(&Yaml::String("dependencies".to_string()))
        .and_then(Yaml::as_hash)
        .and_then(|deps| deps.get(&Yaml::String("flutter".to_string())))
        .and_then(Yaml::as_hash)
        .and_then(|fl| fl.get(&Yaml::String("sdk".to_string())))
        .and_then(Yaml::as_str)
        == Some("flutter")
}

fn workspace_entries(root: &yaml_rust2::yaml::Hash) -> Option<Vec<String>> {
    let seq = root.get(&Yaml::String("workspace".to_string()))?.as_vec()?;
    let entries: Vec<String> = seq.iter().filter_map(Yaml::as_str).map(String::from).collect();
    if entries.is_empty() { None } else { Some(entries) }
}

fn expand_workspace_members(workspace_root: &Path, entries: &[String]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut dirs = Vec::new();

    for raw in entries {
        let entry = raw.trim().trim_end_matches('/');
        if entry.is_empty() {
            continue;
        }
        if entry.contains('*') {
            for expanded in expand_simple_glob(workspace_root, entry) {
                if expanded.is_dir() && seen.insert(expanded.clone()) {
                    dirs.push(expanded);
                }
            }
        } else {
            let candidate = workspace_root.join(entry);
            if candidate.is_dir() && seen.insert(candidate.clone()) {
                dirs.push(candidate);
            }
        }
    }

    dirs
}

/// Expands single-level wildcard segments (e.g. `apps/*`) in Dart `workspace:` entries.
fn expand_simple_glob(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let mut frontier = vec![root.to_path_buf()];

    for seg in segments {
        let mut next = Vec::new();
        for base in frontier {
            if seg.contains('*') {
                if let Ok(read_dir) = std::fs::read_dir(&base) {
                    let mut children: Vec<PathBuf> = read_dir
                        .flatten()
                        .filter_map(|e| {
                            let name = e.file_name();
                            let name_str = name.to_string_lossy();
                            (e.path().is_dir() && wildcard_match(seg, &name_str)).then_some(e.path())
                        })
                        .collect();
                    children.sort();
                    next.extend(children);
                }
            } else {
                let child = base.join(seg);
                if child.is_dir() {
                    next.push(child);
                }
            }
        }
        frontier = next;
    }

    frontier
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    for (idx, part) in parts.iter().enumerate() {
        if idx == 0 {
            let Some(after) = rest.strip_prefix(part) else {
                return false;
            };
            rest = after;
        } else if idx == parts.len() - 1 {
            return rest.ends_with(part);
        } else if let Some(pos) = rest.find(part) {
            rest = &rest[pos + part.len()..];
        } else {
            return false;
        }
    }
    true
}

fn is_flutter_app_dir(dir: &Path, root: &yaml_rust2::yaml::Hash, target: &Path, output: &Path) -> bool {
    if !has_flutter_sdk_dep(root) {
        return false;
    }
    let target_rel = normalize_app_relative(dir, target).unwrap_or_else(|_| target.to_path_buf());
    if dir.join(&target_rel).is_file() {
        return true;
    }
    // When using the default target, still recognize flavor-only apps (`lib/main_dev.dart`, etc.)
    // so the user gets a helpful `--target` message instead of "no Flutter app found".
    if target == Path::new(DEFAULT_TARGET) {
        let output_rel = normalize_app_relative(dir, output).unwrap_or_else(|_| output.to_path_buf());
        return !find_candidate_entrypoints(dir, &output_rel).is_empty();
    }
    false
}

fn find_candidate_entrypoints(app_dir: &Path, output_rel: &Path) -> Vec<PathBuf> {
    let lib_dir = app_dir.join("lib");
    let Ok(entries) = std::fs::read_dir(&lib_dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if (name == "main.dart" || (name.starts_with("main_") && name.ends_with(".dart")))
            && name != "main_tapwing.dart"
        {
            let rel = PathBuf::from("lib").join(name);
            if rel != output_rel {
                found.push(rel);
            }
        }
    }
    found.sort();
    found
}

fn normalize_app_relative(app_dir: &Path, file: &Path) -> Result<PathBuf> {
    let stripped = if file.is_absolute() {
        file.strip_prefix(app_dir)
            .with_context(|| format!("{} must be inside {}", file.display(), app_dir.display()))?
    } else {
        file
    };

    let mut clean = PathBuf::new();
    for comp in stripped.components() {
        match comp {
            Component::CurDir => {}
            Component::Normal(s) => clean.push(s),
            Component::ParentDir => bail!("path must stay inside the app directory: {}", file.display()),
            Component::RootDir | Component::Prefix(_) => {
                bail!("path must be relative to the app directory: {}", file.display())
            }
        }
    }
    if clean.as_os_str().is_empty() {
        bail!("invalid empty file path");
    }
    Ok(clean)
}

fn display_relative(root_dir: &Path, app_dir: &Path) -> Option<String> {
    let rel = app_dir.strip_prefix(root_dir).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() || s == "." { None } else { Some(s) }
}
