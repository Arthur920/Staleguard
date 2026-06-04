//! Shared finding type used by every layer.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// doc claim disagrees with code
    Contradicted,
    /// doc refers to something that no longer exists
    Stale,
    /// could not gather evidence either way
    Unverifiable,
    /// claim backed by code (not reported by default)
    Supported,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Contradicted => "contradicted",
            Verdict::Stale => "stale",
            Verdict::Unverifiable => "unverifiable",
            Verdict::Supported => "supported",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub verdict: Verdict,
    /// the doc assertion under test
    pub claim: String,
    /// where the claim came from (path:line)
    pub doc_path: String,
    /// human-readable explanation
    pub detail: String,
    /// 1 deterministic | 2 retrieval | 3 llm
    pub layer: u8,
    /// supporting / conflicting code references
    pub code_refs: Vec<String>,
}
