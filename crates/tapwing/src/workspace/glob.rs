//! Java `FileSystem.getPathMatcher("glob:…")` semantics (`sun.nio.fs.Globs`,
//! Unix flavour), which Maestro uses for the `flows` patterns of a workspace
//! `config.yaml`:
//!
//! - `*` and `?` stay inside one path segment, `**` crosses segments (and,
//!   like Java's `.*`, stops at line terminators);
//! - `[abc]`, `[a-z]`, `[!a]` classes never match `/`; a leading `^` is a
//!   literal member, not a negation;
//! - `{a,b}` groups do not nest; `}` and `,` outside a group are literals;
//! - `\x` escapes `x`.
//!
//! Patterns are matched against the whole path, as `PathMatcher.matches` does.

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Lit(char),
    /// `*`: `[^/]*`
    Star,
    /// `**`: `.*`
    DoubleStar,
    /// `?`: `[^/]`
    Any,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
    Group(Vec<Vec<Tok>>),
}

/// A compiled glob pattern.
#[derive(Clone, Debug)]
pub struct Glob {
    toks: Vec<Tok>,
}

impl Glob {
    /// Compiles `pattern`, failing where Java throws `PatternSyntaxException`.
    pub fn new(pattern: &str) -> Result<Glob, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut top: Vec<Tok> = Vec::new();
        // Alternatives of the open `{…}` group, and the one being built.
        let mut group: Option<(Vec<Vec<Tok>>, Vec<Tok>)> = None;
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            let tok = match c {
                '\\' => {
                    let Some(&next) = chars.get(i) else { return Err("No character to escape".into()) };
                    i += 1;
                    Tok::Lit(next)
                }
                '[' => {
                    let (tok, next) = class(&chars, i)?;
                    i = next;
                    tok
                }
                '{' => {
                    if group.is_some() {
                        return Err("Cannot nest groups".into());
                    }
                    group = Some((Vec::new(), Vec::new()));
                    continue;
                }
                '}' => match group.take() {
                    Some((mut alts, cur)) => {
                        alts.push(cur);
                        Tok::Group(alts)
                    }
                    None => Tok::Lit('}'),
                },
                ',' => match group.as_mut() {
                    Some((alts, cur)) => {
                        alts.push(std::mem::take(cur));
                        continue;
                    }
                    None => Tok::Lit(','),
                },
                '*' if chars.get(i) == Some(&'*') => {
                    i += 1;
                    Tok::DoubleStar
                }
                '*' => Tok::Star,
                '?' => Tok::Any,
                other => Tok::Lit(other),
            };
            match group.as_mut() {
                Some((_, cur)) => cur.push(tok),
                None => top.push(tok),
            }
        }
        if group.is_some() {
            return Err("Missing '}'".into());
        }
        Ok(Glob { toks: top })
    }

    /// Whether the whole of `path` matches.
    pub fn matches(&self, path: &str) -> bool {
        let s: Vec<char> = path.chars().collect();
        match_at(&self.toks, &s)
    }
}

/// Parses a `[...]` class whose `[` ends at `i`; returns it and the index
/// after its `]`. Mirrors the class branch of `Globs.toRegexPattern`.
fn class(chars: &[char], mut i: usize) -> Result<(Tok, usize), String> {
    let mut negated = false;
    let mut ranges: Vec<(char, char)> = Vec::new();
    match chars.get(i) {
        // `[^…]` is a literal `^`, not a negation (Java escapes it).
        Some('^') => {
            ranges.push(('^', '^'));
            i += 1;
        }
        _ => {
            if chars.get(i) == Some(&'!') {
                negated = true;
                i += 1;
            }
            if chars.get(i) == Some(&'-') {
                ranges.push(('-', '-'));
                i += 1;
            }
        }
    }
    let mut has_range_start = false;
    let mut last = '\0';
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c == ']' {
            return Ok((Tok::Class { negated, ranges }, i));
        }
        if c == '/' {
            return Err("Explicit 'name separator' in class".into());
        }
        if c != '-' {
            ranges.push((c, c));
            has_range_start = true;
            last = c;
            continue;
        }
        if !has_range_start {
            return Err("Invalid range".into());
        }
        let end = chars.get(i).copied();
        i += 1;
        match end {
            // `[a-]`: the `-` stays a literal member and the class closes.
            Some(']') => {
                ranges.push(('-', '-'));
                return Ok((Tok::Class { negated, ranges }, i));
            }
            None => break,
            Some(end) if end < last => return Err("Invalid range".into()),
            Some(end) => {
                if let Some(slot) = ranges.last_mut() {
                    *slot = (last, end);
                }
                has_range_start = false;
            }
        }
    }
    Err("Missing ']'".into())
}

/// Java regex `.` (without `DOTALL`) does not match line terminators.
fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

fn match_at(toks: &[Tok], s: &[char]) -> bool {
    let Some((tok, rest)) = toks.split_first() else { return s.is_empty() };
    match tok {
        Tok::Lit(c) => s.first() == Some(c) && match_at(rest, &s[1..]),
        Tok::Any => s.first().is_some_and(|&c| c != '/') && match_at(rest, &s[1..]),
        Tok::Class { negated, ranges } => {
            s.first().is_some_and(|&c| c != '/' && ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&c)) != *negated)
                && match_at(rest, &s[1..])
        }
        Tok::Star => repeat_then(rest, s, |c| c != '/'),
        Tok::DoubleStar => repeat_then(rest, s, |c| !is_line_terminator(c)),
        Tok::Group(alts) => alts.iter().any(|alt| {
            let mut seq = alt.clone();
            seq.extend_from_slice(rest);
            match_at(&seq, s)
        }),
    }
}

/// Zero or more leading chars satisfying `ok`, then `rest`.
fn repeat_then(rest: &[Tok], s: &[char], ok: impl Fn(char) -> bool) -> bool {
    let mut n = 0;
    loop {
        if match_at(rest, &s[n..]) {
            return true;
        }
        if n == s.len() || !ok(s[n]) {
            return false;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, path: &str) -> bool {
        Glob::new(pattern).unwrap().matches(path)
    }

    #[test]
    fn star_stays_in_segment_double_star_crosses() {
        assert!(m("*", "flowA.yaml"));
        assert!(!m("*", "sub/flowA.yaml"));
        assert!(m("**", "sub/deeper/flowA.yaml"));
        assert!(m("featureA/*", "featureA/flowA.yaml"));
        assert!(!m("featureA/*", "featureA/x/flowA.yaml"));
        // Java's `**/` needs a separator: top-level files do not match.
        assert!(!m("**/*.yaml", "flowA.yaml"));
        assert!(m("**/*.yaml", "a/flowA.yaml"));
        assert!(m("config_dir/**", "config_dir/ignored.yaml"));
    }

    #[test]
    fn classes_groups_and_escapes() {
        assert!(m("flow[AB].yaml", "flowA.yaml"));
        assert!(!m("flow[!AB].yaml", "flowA.yaml"));
        assert!(m("flow[!AB].yaml", "flowC.yaml"));
        assert!(m("flow[a-c].yaml", "flowb.yaml"));
        assert!(m("flow[^].yaml", "flow^.yaml"));
        assert!(m("flow[a-].yaml", "flow-.yaml"));
        assert!(m("{featureA,featureB}/*", "featureB/x.yaml"));
        assert!(!m("{featureA,featureB}/*", "featureC/x.yaml"));
        assert!(m("{a,ab}c", "abc"));
        assert!(m("a,b}", "a,b}"));
        assert!(m("\\*.yaml", "*.yaml"));
        assert!(!m("\\*.yaml", "x.yaml"));
        assert!(m("?.yaml", "a.yaml"));
        assert!(!m("?", "/"));
    }

    #[test]
    fn syntax_errors_like_java() {
        assert_eq!(Glob::new("a\\").unwrap_err(), "No character to escape");
        assert_eq!(Glob::new("{a,{b}}").unwrap_err(), "Cannot nest groups");
        assert_eq!(Glob::new("{a,b").unwrap_err(), "Missing '}'");
        assert_eq!(Glob::new("[ab").unwrap_err(), "Missing ']'");
        assert_eq!(Glob::new("[a/b]").unwrap_err(), "Explicit 'name separator' in class");
        assert_eq!(Glob::new("[z-a]").unwrap_err(), "Invalid range");
        assert_eq!(Glob::new("[^-a]").unwrap_err(), "Invalid range");
    }
}
