//! Minimal unified diff generator for `tapwing init --dry-run`.

const CONTEXT_LINES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffOp {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// Renders a standard unified diff between `old` and `new`.
///
/// Returns an empty string when `old == new`.
pub(crate) fn unified_diff(old_label: &str, new_label: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }

    let old_lines: Vec<&str> = split_lines(old);
    let new_lines: Vec<&str> = split_lines(new);

    let ops = compute_diff_ops(&old_lines, &new_lines);
    let hunks = group_hunks(&ops, CONTEXT_LINES);
    if hunks.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str(&format!("--- {old_label}\n+++ {new_label}\n"));
    for hunk in hunks {
        let (old_start, old_count, new_start, new_count) = hunk_range(&ops[hunk.clone()], &old_lines, &new_lines);
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            format_range(old_start, old_count),
            format_range(new_start, new_count)
        ));
        for &op in &ops[hunk] {
            match op {
                DiffOp::Equal(i, _) => {
                    out.push(' ');
                    out.push_str(old_lines[i]);
                    out.push('\n');
                }
                DiffOp::Delete(i) => {
                    out.push('-');
                    out.push_str(old_lines[i]);
                    out.push('\n');
                }
                DiffOp::Insert(j) => {
                    out.push('+');
                    out.push_str(new_lines[j]);
                    out.push('\n');
                }
            }
        }
    }
    out
}

fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    text.lines().collect()
}

fn compute_diff_ops(old: &[&str], new: &[&str]) -> Vec<DiffOp> {
    let n = old.len();
    let m = new.len();

    // Fast path for brand-new or fully deleted files.
    if n == 0 {
        return (0..m).map(DiffOp::Insert).collect();
    }
    if m == 0 {
        return (0..n).map(DiffOp::Delete).collect();
    }

    // Trim shared prefix and suffix to keep the DP table tiny even on large pubspecs.
    let mut prefix = 0;
    while prefix < n && prefix < m && old[prefix] == new[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < (n - prefix) && suffix < (m - prefix) && old[n - 1 - suffix] == new[m - 1 - suffix] {
        suffix += 1;
    }

    let old_mid = &old[prefix..n - suffix];
    let new_mid = &new[prefix..m - suffix];
    let on = old_mid.len();
    let nm = new_mid.len();

    let mut ops = Vec::with_capacity(n + m);
    for i in 0..prefix {
        ops.push(DiffOp::Equal(i, i));
    }

    // Guard against pathological huge files (fallback: replace whole middle block).
    if on.saturating_mul(nm) > 2_000_000 {
        for i in 0..on {
            ops.push(DiffOp::Delete(prefix + i));
        }
        for j in 0..nm {
            ops.push(DiffOp::Insert(prefix + j));
        }
    } else {
        // Standard LCS DP table from back to front so backtracking is forward.
        let cols = nm + 1;
        let mut dp = vec![0u32; (on + 1) * cols];
        for i in (0..on).rev() {
            for j in (0..nm).rev() {
                dp[i * cols + j] = if old_mid[i] == new_mid[j] {
                    dp[(i + 1) * cols + (j + 1)] + 1
                } else {
                    dp[(i + 1) * cols + j].max(dp[i * cols + (j + 1)])
                };
            }
        }

        let (mut i, mut j) = (0, 0);
        while i < on && j < nm {
            if old_mid[i] == new_mid[j] {
                ops.push(DiffOp::Equal(prefix + i, prefix + j));
                i += 1;
                j += 1;
            } else if dp[(i + 1) * cols + j] >= dp[i * cols + (j + 1)] {
                ops.push(DiffOp::Delete(prefix + i));
                i += 1;
            } else {
                ops.push(DiffOp::Insert(prefix + j));
                j += 1;
            }
        }
        while i < on {
            ops.push(DiffOp::Delete(prefix + i));
            i += 1;
        }
        while j < nm {
            ops.push(DiffOp::Insert(prefix + j));
            j += 1;
        }
    }

    for k in 0..suffix {
        ops.push(DiffOp::Equal(n - suffix + k, m - suffix + k));
    }
    ops
}

fn group_hunks(ops: &[DiffOp], context: usize) -> Vec<std::ops::Range<usize>> {
    let mut hunks: Vec<std::ops::Range<usize>> = Vec::new();
    let len = ops.len();
    let mut idx = 0;

    while idx < len {
        if matches!(ops[idx], DiffOp::Equal(_, _)) {
            idx += 1;
            continue;
        }
        let start = idx.saturating_sub(context);
        let mut end = idx + 1;
        let mut equal_run = 0;
        while end < len {
            if matches!(ops[end], DiffOp::Equal(_, _)) {
                equal_run += 1;
                if equal_run > context * 2 {
                    break;
                }
            } else {
                equal_run = 0;
            }
            end += 1;
        }
        let hunk_end = if equal_run > context * 2 { end - equal_run + context } else { end };
        hunks.push(start..hunk_end.min(len));
        idx = hunk_end;
    }

    hunks
}

fn hunk_range(hunk: &[DiffOp], old_lines: &[&str], new_lines: &[&str]) -> (usize, usize, usize, usize) {
    let mut old_first: Option<usize> = None;
    let mut new_first: Option<usize> = None;
    let mut old_count = 0usize;
    let mut new_count = 0usize;

    for &op in hunk {
        match op {
            DiffOp::Equal(i, j) => {
                old_first.get_or_insert(i + 1);
                new_first.get_or_insert(j + 1);
                old_count += 1;
                new_count += 1;
            }
            DiffOp::Delete(i) => {
                old_first.get_or_insert(i + 1);
                old_count += 1;
            }
            DiffOp::Insert(j) => {
                new_first.get_or_insert(j + 1);
                new_count += 1;
            }
        }
    }

    let old_start = match (old_first, old_count) {
        (Some(s), _) => s,
        (None, 0) if old_lines.is_empty() => 0,
        (None, _) => 1,
    };
    let new_start = match (new_first, new_count) {
        (Some(s), _) => s,
        (None, 0) if new_lines.is_empty() => 0,
        (None, _) => 1,
    };

    (old_start, old_count, new_start, new_count)
}

fn format_range(start: usize, count: usize) -> String {
    if count == 1 { format!("{start}") } else { format!("{start},{count}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_file_diff_lists_all_lines_as_added() {
        let diff = unified_diff("/dev/null", "b/lib/main_tapwing.dart", "", "void main() {\n}\n");
        assert!(diff.contains("--- /dev/null\n+++ b/lib/main_tapwing.dart\n"));
        assert!(diff.contains("@@ -0,0 +1,2 @@\n"));
        assert!(diff.contains("+void main() {\n+}\n"));
    }

    #[test]
    fn modified_file_diff_shows_context_and_addition() {
        let old = "name: app\ndev_dependencies:\n  flutter_test:\n    sdk: flutter\nflutter:\n";
        let new = "name: app\ndev_dependencies:\n  flutter_test:\n    sdk: flutter\n  tapwing_driver:\n    path: ../driver\nflutter:\n";
        let diff = unified_diff("a/pubspec.yaml", "b/pubspec.yaml", old, new);
        assert!(diff.contains("--- a/pubspec.yaml\n+++ b/pubspec.yaml\n"));
        assert!(diff.contains("     sdk: flutter\n+  tapwing_driver:\n+    path: ../driver\n flutter:\n"));
    }

    #[test]
    fn identical_files_produce_empty_diff() {
        assert_eq!(unified_diff("a/x", "b/x", "hello\n", "hello\n"), "");
    }
}
