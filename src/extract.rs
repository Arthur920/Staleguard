//! Pull verifiable claims out of markdown docs.
//!
//! For now this only surfaces the deterministically checkable claims (paths).
//! Layer-3 free-text claim extraction (LLM) plugs in alongside these.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone)]
pub struct PathClaim {
    /// the quoted token, e.g. "src/index.ts"
    pub raw: String,
    /// markdown file it appeared in
    pub doc_path: String,
    pub line: usize,
}

/// `backtick-quoted` tokens that look like a relative file or dir path.
fn path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([\w./\-]+/[\w./\-]+|[\w\-]+\.[\w]{1,5})`").unwrap())
}

/// Find backtick-quoted tokens that look like paths the repo should contain.
pub fn extract_path_claims(markdown: &str, doc_path: &str) -> Vec<PathClaim> {
    let mut claims = Vec::new();
    for (i, line) in markdown.lines().enumerate() {
        for cap in path_re().captures_iter(line) {
            let token = &cap[1];
            // Skip obvious non-paths (URLs, version specifiers, globs).
            if token.contains("://") || token.starts_with('*') || token.ends_with("/*") {
                continue;
            }
            claims.push(PathClaim {
                raw: token.to_string(),
                doc_path: doc_path.to_string(),
                line: i + 1,
            });
        }
    }
    claims
}
