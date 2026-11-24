//! Picocli-compatible `@argfile` expansion for the CLI (`maestro test @args.txt`).
//!
//! Maestro's CLI is built on Picocli, which expands arguments starting with `@`
//! from a file (e.g. `maestro test @maestro/stagingTestArgs flow.yaml`).
//! Supporting the same syntax lets existing CI scripts and Makefiles swap
//! `maestro` for `tapwing` without rewriting argument files.

use anyhow::{Context, Result, bail};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// Guard against accidentally deep `@file` nesting.
const MAX_ARGFILE_DEPTH: usize = 16;

/// Expands `@file` arguments in `raw_args` (preserving `argv[0]`).
///
/// Follows Picocli's `@argfile` conventions:
/// - `@@foo` is unescaped to a literal `@foo`.
/// - Arguments after a standalone `--` are passed through verbatim.
/// - Inside an argfile, whitespace separates tokens, `#` starts a line comment
///   outside quotes, single/double quotes group tokens, and `\` escapes the
///   next character outside single quotes (or `\"` / `\\` inside double quotes).
/// - Nested `@file` entries resolve relative to the current working directory
///   (matching Picocli), with cycle detection and a max depth of 16.
pub fn expand_argfiles<I, S>(raw_args: I) -> Result<Vec<OsString>>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut iter = raw_args.into_iter().map(Into::into);
    let mut out = Vec::new();
    if let Some(program) = iter.next() {
        out.push(program);
    }
    let rest: Vec<OsString> = iter.collect();
    let mut active = HashSet::new();
    expand_slice(&rest, 0, &mut active, &mut out)?;
    Ok(out)
}

fn expand_slice(args: &[OsString], depth: usize, active: &mut HashSet<PathBuf>, out: &mut Vec<OsString>) -> Result<()> {
    let mut literal_only = false;
    for arg in args {
        if literal_only {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            literal_only = true;
            out.push(arg.clone());
            continue;
        }
        let Some(s) = arg.to_str() else {
            out.push(arg.clone());
            continue;
        };
        if let Some(rest) = s.strip_prefix("@@") {
            out.push(OsString::from(format!("@{rest}")));
        } else if let Some(path_str) = s.strip_prefix('@')
            && !path_str.is_empty()
        {
            if depth >= MAX_ARGFILE_DEPTH {
                bail!("argument file nesting exceeded maximum depth ({MAX_ARGFILE_DEPTH}): @{path_str}");
            }
            let path = Path::new(path_str);
            let canonical =
                fs::canonicalize(path).with_context(|| format!("failed to read argument file `{path_str}`"))?;
            if !active.insert(canonical.clone()) {
                bail!("cyclic argument file reference: `{path_str}`");
            }
            let content =
                fs::read_to_string(&canonical).with_context(|| format!("failed to read argument file `{path_str}`"))?;
            let tokens = tokenize_argfile(&content, path_str)?;
            let nested: Vec<OsString> = tokens.into_iter().map(OsString::from).collect();
            let res = expand_slice(&nested, depth + 1, active, out);
            active.remove(&canonical);
            res?;
        } else {
            out.push(arg.clone());
        }
    }
    Ok(())
}

fn tokenize_argfile(content: &str, label: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = content.chars().peekable();

    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    current.push(c);
                }
            }
            Some(_) => match c {
                '"' => quote = None,
                '\\' => match chars.peek().copied() {
                    Some('"' | '\\') => {
                        if let Some(escaped) = chars.next() {
                            current.push(escaped);
                        }
                    }
                    Some('\n') => {
                        chars.next();
                    }
                    Some('\r') => {
                        chars.next();
                        if chars.peek() == Some(&'\n') {
                            chars.next();
                        }
                    }
                    _ => current.push('\\'),
                },
                _ => current.push(c),
            },
            None => match c {
                '#' => {
                    if in_token {
                        tokens.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                    for ch in chars.by_ref() {
                        if ch == '\n' {
                            break;
                        }
                    }
                }
                c if c.is_whitespace() => {
                    if in_token {
                        tokens.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                }
                '\'' | '"' => {
                    quote = Some(c);
                    in_token = true;
                }
                '\\' => {
                    in_token = true;
                    match chars.next() {
                        Some('\n') => {}
                        Some('\r') => {
                            if chars.peek() == Some(&'\n') {
                                chars.next();
                            }
                        }
                        Some(escaped) => current.push(escaped),
                        None => current.push('\\'),
                    }
                }
                _ => {
                    current.push(c);
                    in_token = true;
                }
            },
        }
    }

    if let Some(q) = quote {
        bail!("unterminated `{q}` quote in argument file `{label}`");
    }
    if in_token {
        tokens.push(current);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tapwing-argfile-{name}-{}-{id}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn as_strings(v: Vec<OsString>) -> Vec<String> {
        v.into_iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn expands_maestro_style_env_argfile() {
        let dir = temp_dir("basic");
        let file = dir.join("stagingTestArgs");
        fs::write(
            &file,
            "# Staging arguments\n-e APPID=com.example.stage\n-e \"LABEL=Hello World\"\n-e 'PASS=a#b c'\n",
        )
        .unwrap();

        let expanded = expand_argfiles([
            "tapwing".to_string(),
            "test".to_string(),
            format!("@{}", file.display()),
            "flow.yaml".to_string(),
        ])
        .unwrap();

        assert_eq!(
            as_strings(expanded),
            vec![
                "tapwing",
                "test",
                "-e",
                "APPID=com.example.stage",
                "-e",
                "LABEL=Hello World",
                "-e",
                "PASS=a#b c",
                "flow.yaml",
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn supports_double_at_escape_and_double_dash_passthrough() {
        let expanded = expand_argfiles(["tapwing", "test", "@@literal", "--", "@not-a-file"]).unwrap();
        assert_eq!(as_strings(expanded), vec!["tapwing", "test", "@literal", "--", "@not-a-file"]);
    }

    #[test]
    fn detects_cyclic_argfile_references() {
        let dir = temp_dir("cycle");
        let a = dir.join("a.args");
        let b = dir.join("b.args");
        fs::write(&a, format!("@{}\n", b.display())).unwrap();
        fs::write(&b, format!("@{}\n", a.display())).unwrap();

        let err = expand_argfiles(["tapwing".to_string(), format!("@{}", a.display())]).unwrap_err();
        assert!(err.to_string().contains("cyclic"), "unexpected error: {err:#}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn errors_clearly_on_missing_argfile() {
        let err = expand_argfiles(["tapwing", "test", "@/nonexistent/tapwing_argfile_missing"]).unwrap_err();
        assert!(err.to_string().contains("failed to read argument file"));
    }

    #[test]
    fn preserves_empty_quoted_argument_and_errors_on_unterminated_quote() {
        let tokens = tokenize_argfile("-e \"\" -e ''", "inline").unwrap();
        assert_eq!(tokens, vec!["-e", "", "-e", ""]);

        let err = tokenize_argfile("-e \"unterminated", "inline").unwrap_err();
        assert!(err.to_string().contains("unterminated"));
    }
}
