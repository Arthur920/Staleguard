//! Git access for the drift layer (Layer 0), via the `git` CLI.
//!
//! Everything here degrades safely: a non-git directory, a missing base ref, or
//! shallow history makes each function return `None`/empty, and the caller falls
//! back to a full scan. We never crash and never invent a result — lineage is a
//! narrowing optimization *behind* the full scan, so under-reporting a change
//! only means "re-check more," never "miss a check".

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use crate::code::CodeIndex;

/// Run `git <args>` in `root` and return trimmed stdout, or `None` if git is
/// absent, the command failed, or output wasn't UTF-8.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// `true` if `root` is inside a git work tree.
pub fn is_repo(root: &Path) -> bool {
    git(root, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s.trim() == "true")
        .unwrap_or(false)
}

/// The current HEAD commit sha.
pub fn head_sha(root: &Path) -> Option<String> {
    git(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string())
}

/// One file's change between `base` and the working tree: its new-side path, the
/// path it was renamed from (if any), and the new-side line ranges that changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub renamed_from: Option<String>,
    /// Inclusive `(start_line, end_line)` ranges on the new side.
    pub ranges: Vec<(usize, usize)>,
}

/// Per-file changed line ranges between `base` and the working tree, with rename
/// detection (`-M`) and zero context (`-U0`). Empty when git/base is unavailable.
pub fn changed_lines(root: &Path, base: &str) -> Vec<FileDiff> {
    let Some(text) = git(root, &["diff", "-U0", "-M", "--no-color", base, "--"]) else {
        return Vec::new();
    };
    parse_diff(&text)
}

fn parse_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `a/<old> b/<new>` — take the new-side path as the default.
            let path = rest
                .split(" b/")
                .nth(1)
                .map(str::to_string)
                .unwrap_or_default();
            files.push(FileDiff {
                path,
                renamed_from: None,
                ranges: Vec::new(),
            });
        } else if let Some(old) = line.strip_prefix("rename from ") {
            if let Some(f) = files.last_mut() {
                f.renamed_from = Some(old.to_string());
            }
        } else if let Some(new) = line.strip_prefix("rename to ") {
            if let Some(f) = files.last_mut() {
                f.path = new.to_string();
            }
        } else if let Some(new) = line.strip_prefix("+++ b/") {
            if let Some(f) = files.last_mut() {
                f.path = new.to_string();
            }
        } else if line.starts_with("@@") {
            if let Some((start, len)) = parse_hunk_new_side(line) {
                if len > 0 {
                    if let Some(f) = files.last_mut() {
                        f.ranges.push((start, start + len - 1));
                    }
                }
            }
        }
    }
    files.retain(|f| !f.path.is_empty());
    files
}

/// Parse the new-side `(start, len)` from a hunk header `@@ -a,b +c,d @@`.
/// A missing `,d` means length 1.
fn parse_hunk_new_side(header: &str) -> Option<(usize, usize)> {
    let plus = header.split('+').nth(1)?;
    let token = plus.split([' ', ',']).next()?;
    let start: usize = token.parse().ok()?;
    let len = match plus.split(',').nth(1) {
        Some(rest) => rest
            .split([' ', '@'])
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(1),
        None => 1,
    };
    Some((start, len))
}

/// The set of symbol identity keys (qualified name, bare name, module) for every
/// symbol touched by `diffs`. A symbol is touched if a changed range on its file
/// overlaps its body, or its file was renamed (the whole file moved). The set is
/// intentionally over-approximate (includes coarse keys) so a claim anchored by
/// any facet still invalidates.
pub fn changed_symbols(index: &CodeIndex, diffs: &[FileDiff]) -> HashSet<String> {
    let mut changed = HashSet::new();
    for d in diffs {
        for s in &index.symbols {
            if s.body_span.path != d.path {
                continue;
            }
            let moved = d.renamed_from.is_some();
            let edited = d
                .ranges
                .iter()
                .any(|&(a, b)| overlaps(s.body_span.start_line, s.body_span.end_line, a, b));
            if moved || edited {
                changed.insert(s.qualified_name.clone());
                changed.insert(s.name.clone());
                changed.insert(s.module.clone());
            }
        }
        // Also record the raw file/module paths so path-anchored claims invalidate.
        changed.insert(d.path.clone());
        if let Some(old) = &d.renamed_from {
            changed.insert(old.clone());
        }
    }
    changed
}

fn overlaps(a0: usize, a1: usize, b0: usize, b1: usize) -> bool {
    a0 <= b1 && b0 <= a1
}

/// Per-commit file sets from history (newest first), capped at `max_commits`.
/// Used to mine change-coupling. Empty when git is unavailable.
pub fn file_change_history(root: &Path, max_commits: usize) -> Vec<Vec<String>> {
    // \x1e (record sep) marks a commit boundary, then the file list follows.
    let cap = format!("-{max_commits}");
    let Some(text) = git(
        root,
        &["log", &cap, "--name-only", "--pretty=format:\x1e%H"],
    ) else {
        return Vec::new();
    };
    let mut commits = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut started = false;
    for line in text.lines() {
        if let Some(_sha) = line.strip_prefix('\x1e') {
            if started {
                commits.push(std::mem::take(&mut current));
            }
            started = true;
        } else if !line.trim().is_empty() {
            current.push(line.trim().to_string());
        }
    }
    if started {
        commits.push(current);
    }
    commits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hunk_headers() {
        assert_eq!(parse_hunk_new_side("@@ -1,2 +3,4 @@ fn x"), Some((3, 4)));
        assert_eq!(parse_hunk_new_side("@@ -1 +5 @@"), Some((5, 1)));
        assert_eq!(parse_hunk_new_side("@@ -1,0 +2,0 @@"), Some((2, 0)));
    }

    #[test]
    fn parses_modified_file_diff() {
        let diff = "diff --git a/src/x.rs b/src/x.rs\n\
                    index abc..def 100644\n\
                    --- a/src/x.rs\n\
                    +++ b/src/x.rs\n\
                    @@ -10,2 +10,3 @@ fn foo\n\
                    +    let y = 5;\n";
        let files = parse_diff(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/x.rs");
        assert_eq!(files[0].renamed_from, None);
        assert_eq!(files[0].ranges, vec![(10, 12)]);
    }

    #[test]
    fn parses_rename() {
        let diff = "diff --git a/old.rs b/new.rs\n\
                    similarity index 95%\n\
                    rename from old.rs\n\
                    rename to new.rs\n";
        let files = parse_diff(diff);
        assert_eq!(files[0].path, "new.rs");
        assert_eq!(files[0].renamed_from, Some("old.rs".to_string()));
        assert!(files[0].ranges.is_empty());
    }

    #[test]
    fn overlap_logic() {
        assert!(overlaps(10, 20, 15, 18));
        assert!(overlaps(10, 20, 20, 25));
        assert!(!overlaps(10, 20, 21, 25));
    }
}
