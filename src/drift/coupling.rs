//! Git change-coupling between docs and code (no model, no embeddings).
//!
//! Only the net-new coverage signal lives here: code no doc has ever changed
//! alongside. (A doc-lags-code "staleness prior" was dropped: bulk commits
//! coupled nearly every doc to every hot file, so it was all noise in the wild.)

use std::collections::HashSet;

/// Commits to mine from history (newest first). The single source of truth for
/// how deep every history-mining pass reads, so the shared fetch in `run_check`
/// covers them.
pub const MAX_COMMITS: usize = 1000;

/// Code files that have **never** co-changed with any doc across `history`: the
/// "no doc has ever tracked this code" signal for net-new coverage gaps
/// (`coverage-gaps.md` §4). Conservative: any commit touching both a doc and a
/// code file marks *all* its code files tracked, so we under-flag rather than
/// over-flag. Empty when git history is unavailable.
pub fn code_without_codoc(history: &[Vec<String>]) -> HashSet<String> {
    let mut all_code: HashSet<&str> = HashSet::new();
    let mut tracked: HashSet<&str> = HashSet::new();
    for files in history {
        let has_doc = files.iter().any(|f| is_doc(f));
        for f in files {
            if is_doc(f) {
                continue;
            }
            all_code.insert(f.as_str());
            if has_doc {
                tracked.insert(f.as_str());
            }
        }
    }
    all_code
        .into_iter()
        .filter(|f| !tracked.contains(f))
        .map(str::to_string)
        .collect()
}

fn is_doc(path: &str) -> bool {
    path.ends_with(".md") || path.ends_with(".markdown")
}
