//! "did you mean …?" hints on stale findings, so a finding is a one-line fix
//! rather than a hunt: a missing path's git rename (or the one file elsewhere
//! with the same name), and the closest defined script or code identifier for
//! a missing script or env var. Hints only touch `detail`, never the claim or
//! provenance, so `--diff` still matches findings by claim.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::findings::{Finding, Verdict};

pub fn annotate(
    findings: &mut [Finding],
    root: &Path,
    repo_files: &[String],
    code_tokens: &HashSet<String>,
    scripts: &HashSet<String>,
) {
    // Rename history is only fetched when some path is actually stale.
    let mut renames: Option<HashMap<String, String>> = None;
    for f in findings.iter_mut().filter(|f| f.verdict == Verdict::Stale) {
        let hint = if let Some(p) = ticked(&f.detail, "path `") {
            let renames = renames.get_or_insert_with(|| crate::git::renames(root));
            renamed(p, root, renames).or_else(|| same_name(p, root, repo_files))
        } else if let Some(s) = ticked(&f.detail, "script `") {
            closest(s, scripts.iter())
        } else if let Some(name) = ticked(&f.claim, "references env var `") {
            // Same first segment, so `VITE_MAX_FILE_SIZE` never suggests
            // an unrelated `LIMIT_FILE_SIZE`.
            let head = name.split('_').next().unwrap_or(name);
            closest(
                name,
                code_tokens
                    .iter()
                    .filter(|t| is_screaming(t) && t.split('_').next() == Some(head)),
            )
        } else {
            None
        };
        if let Some(h) = hint {
            f.detail.push_str(&format!("; did you mean `{h}`?"));
        }
    }
}

/// The backticked text right after `prefix` in `s`.
fn ticked<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = &s[s.find(prefix)? + prefix.len()..];
    rest.split('`').next()
}

/// Follow `old → new` renames (newest wins) from the path the doc names to one
/// that still exists. The doc may name a path relative to a sub-package, so a
/// rename matches if its old path ends with the doc's path on a segment boundary.
fn renamed(raw: &str, root: &Path, renames: &HashMap<String, String>) -> Option<String> {
    let raw = raw.trim_start_matches("./");
    let mut cur = renames
        .iter()
        .find(|(old, _)| *old == raw || old.ends_with(&format!("/{raw}")))
        .map(|(_, new)| new.clone())?;
    for _ in 0..10 {
        match renames.get(&cur) {
            Some(next) => cur = next.clone(),
            None => break,
        }
    }
    root.join(&cur).exists().then_some(cur)
}

/// The tracked repo file sharing the missing path's file name: the only one, or
/// the one whose directories share the most words with the doc's path
/// (`routers/teams/x.ts` → `team-router/x.ts`). Short generic stems (`api.ts`,
/// `index.ts`) are too common to guess from.
fn same_name(raw: &str, root: &Path, repo_files: &[String]) -> Option<String> {
    let p = Path::new(raw);
    if p.file_stem()?.len() < 6 {
        return None;
    }
    let name = p.file_name()?.to_str()?;
    let rel = |f: &String| {
        let f = Path::new(f);
        f.strip_prefix(root)
            .unwrap_or(f)
            .to_string_lossy()
            .into_owned()
    };
    let mut hits: Vec<String> = repo_files
        .iter()
        .filter(|f| Path::new(f).file_name().and_then(|n| n.to_str()) == Some(name))
        .map(rel)
        .collect();
    if hits.len() > 1 {
        // Tool scratch copies (`.stryker-tmp/`) are not real alternatives.
        let ignored = crate::git::ignored(root, &hits);
        hits.retain(|h| !ignored.contains(h));
    }
    if hits.len() == 1 {
        return hits.pop();
    }
    let want = dir_words(raw);
    let mut scored: Vec<(usize, String)> = hits
        .into_iter()
        .map(|h| (dir_words(&h).intersection(&want).count(), h))
        .collect();
    scored.sort_by_key(|s| std::cmp::Reverse(s.0));
    match scored.as_slice() {
        [(best, h), rest @ ..] if *best > 0 && rest.first().is_none_or(|r| r.0 < *best) => {
            Some(h.clone())
        }
        _ => None,
    }
}

/// Lowercased words of a path's directories, plural `s` dropped, so `teams`
/// and `team-router` share `team`.
fn dir_words(path: &str) -> HashSet<String> {
    let dir = Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned());
    dir.unwrap_or_default()
        .split(['/', '-', '_', '.'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let w = w.to_ascii_lowercase();
            w.strip_suffix('s').map(str::to_string).unwrap_or(w)
        })
        .collect()
}

/// The candidate nearest to `name` by edit distance, within a third of its
/// length, so `AUTHENTICATION_ERROR` finds `AUTHENTICATION_FAILED` but
/// `demo:setup` finds nothing rather than an unrelated script.
fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a String>) -> Option<String> {
    let max = (name.chars().count() / 3).max(1);
    candidates
        .filter(|c| c.as_str() != name && c.len().abs_diff(name.len()) <= max)
        .map(|c| (levenshtein(name, c), c))
        .filter(|(d, _)| *d <= max)
        .min()
        .map(|(_, c)| c.clone())
}

fn is_screaming(t: &str) -> bool {
    t.len() > 3
        && t.contains('_')
        && t.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stale(claim: &str, detail: &str) -> Finding {
        Finding::problem(Verdict::Stale, claim, "README.md:1", detail)
    }

    #[test]
    fn hints_scripts_env_vars_and_moved_files() {
        let dir = std::env::temp_dir().join(format!("staleguard-suggest-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src/unit")).unwrap();
        let moved = dir
            .join("src/unit/Dhl.spec.ts")
            .to_string_lossy()
            .into_owned();
        let tokens: HashSet<String> = ["AUTHENTICATION_FAILED", "AUTHORIZATION_FAILED"]
            .map(String::from)
            .into();
        let scripts: HashSet<String> = ["type-check", "dev"].map(String::from).into();
        let mut fs = vec![
            stale(
                "runs `npm run typecheck`",
                "script `typecheck` is not defined",
            ),
            stale(
                "runs `npm run demo:setup`",
                "script `demo:setup` is not defined",
            ),
            stale(
                "references env var `AUTHENTICATION_ERROR`",
                "env var `AUTHENTICATION_ERROR` is not used in the code",
            ),
            stale(
                "references `src/Dhl.spec.ts`",
                "path `src/Dhl.spec.ts` does not exist",
            ),
        ];
        annotate(&mut fs, &dir, &[moved], &tokens, &scripts);
        assert!(fs[0].detail.ends_with("did you mean `type-check`?"));
        assert!(!fs[1].detail.contains("did you mean"));
        assert!(fs[2]
            .detail
            .ends_with("did you mean `AUTHENTICATION_FAILED`?"));
        assert!(fs[3]
            .detail
            .ends_with("did you mean `src/unit/Dhl.spec.ts`?"));
    }

    #[test]
    fn same_name_breaks_ties_by_directory_words() {
        let files: Vec<String> = [
            "packages/lib/server-only/team/create-team.ts",
            "packages/trpc/server/team-router/create-team.ts",
        ]
        .map(String::from)
        .into();
        let root = Path::new("/nonexistent");
        assert_eq!(
            same_name("routers/teams/create-team.ts", root, &files).as_deref(),
            Some("packages/trpc/server/team-router/create-team.ts")
        );
        // No shared words: no guess.
        assert_eq!(same_name("other/create-team.ts", root, &files), None);
    }

    #[test]
    fn follows_rename_chains_to_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("staleguard-rename-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("pkg/src")).unwrap();
        std::fs::write(dir.join("pkg/src/c.ts"), "").unwrap();
        let renames: HashMap<String, String> = [
            ("pkg/src/a.ts".into(), "pkg/src/b.ts".into()),
            ("pkg/src/b.ts".into(), "pkg/src/c.ts".into()),
        ]
        .into();
        assert_eq!(
            renamed("src/a.ts", &dir, &renames).as_deref(),
            Some("pkg/src/c.ts")
        );
        assert_eq!(renamed("src/zzz.ts", &dir, &renames), None);
    }
}
