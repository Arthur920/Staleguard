//! Git access via the `git` CLI. Everything degrades safely: a non-git
//! directory, a missing ref, or shallow history returns `None`/empty.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// The subset of `paths` (repo-relative) that `.gitignore` rules exclude.
/// Empty outside a git repo or when git is absent.
pub fn ignored(root: &Path, paths: &[String]) -> HashSet<String> {
    if paths.is_empty() {
        return HashSet::new();
    }
    let Ok(mut child) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--no-index", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return HashSet::new();
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(paths.join("\n").as_bytes());
    }
    child
        .wait_with_output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A detached, throwaway checkout of some revision, removed on drop.
pub struct Worktree {
    repo: PathBuf,
    dir: PathBuf,
    /// The checkout's counterpart of the `root` it was made from (same
    /// subdirectory when `root` isn't the repo top level).
    pub root: PathBuf,
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let dir = self.dir.to_string_lossy().into_owned();
        let _ = git(&self.repo, &["worktree", "remove", "--force", &dir]);
    }
}

/// Check `rev` out into a temp worktree. `None` if `rev` doesn't resolve (e.g.
/// a shallow CI clone) or git is unavailable.
pub fn worktree(root: &Path, rev: &str) -> Option<Worktree> {
    let prefix = git(root, &["rev-parse", "--show-prefix"])?;
    let dir = std::env::temp_dir().join(format!("staleguard-base-{}", std::process::id()));
    let d = dir.to_string_lossy().into_owned();
    git(root, &["worktree", "add", "--detach", "--force", &d, rev])?;
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    Some(Worktree {
        repo: root.to_path_buf(),
        root: dir.join(prefix.trim()),
        dir,
    })
}

/// `old → new` for every file rename in the last 2000 commits. Newest wins
/// when a path was renamed more than once.
pub fn renames(root: &Path) -> HashMap<String, String> {
    let Some(text) = git(
        root,
        &[
            "log",
            "-2000",
            "-M",
            "--diff-filter=R",
            "--name-status",
            "--format=",
        ],
    ) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut cols = line.split('\t');
        if let (Some(status), Some(old), Some(new)) = (cols.next(), cols.next(), cols.next()) {
            if status.starts_with('R') {
                map.entry(old.to_string())
                    .or_insert_with(|| new.to_string());
            }
        }
    }
    map
}
