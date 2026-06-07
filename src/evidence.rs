//! Model-free evidence selection for the Layer 3 judge.
//!
//! Layer 2's embedding retrieval re-discovers, by cosine similarity, code that
//! Layer 1 has often *already* resolved: a claim's backtick tokens are grounded
//! to exact symbols/modules during extraction ([`crate::judge::candidate_claims`]
//! -> `ground_claim`). This module turns that grounding into the NLI premise
//! directly — read the resolved symbol bodies — and falls back to a lexical
//! (idf-weighted) match over the public symbol table when a claim grounded to
//! nothing. No embedding model, no whole-corpus pass.
//!
//! Embedding stays available behind `SHLOMES_EMBED_RETRIEVE=1` for claims whose
//! relevant code is genuinely semantic (named by behaviour, not by identifier).

use std::collections::HashMap;
use std::path::Path;

use crate::claim::Provenance;
use crate::code::symbol::Visibility;
use crate::code::CodeIndex;

/// One premise chunk for the judge: code text plus where it came from.
pub struct Evidence {
    pub text: String,
    pub path: String,
    pub start_line: usize,
    pub score: f32,
}

/// Cap on body lines pulled per symbol. The NLI cross-encoder truncates the pair
/// to its token window anyway; this keeps tokenization bounded for huge bodies.
const MAX_BODY_LINES: usize = 50;
/// Below this lexical score a fallback match is too weak to be evidence.
const MIN_LEXICAL_SCORE: f32 = 0.5;

/// Lazily-read file lines, shared across claims (`None` = unreadable).
pub type FileCache = HashMap<String, Option<Vec<String>>>;

/// Gather evidence for one claim: grounded symbols/modules first, then a lexical
/// fallback over the public symbol table if nothing grounded. Returns up to `k`,
/// best first.
pub fn gather(
    claim_text: &str,
    prov: &Provenance,
    index: &CodeIndex,
    lexicon: &Lexicon,
    root: &Path,
    k: usize,
    files: &mut FileCache,
) -> Vec<Evidence> {
    let mut out: Vec<Evidence> = Vec::new();
    let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();

    // 1. Symbols the claim grounded to (exact, preferred).
    for qn in &prov.symbols {
        for sym in index.symbols.iter().filter(|s| &s.qualified_name == qn) {
            if let Some(ev) = symbol_evidence(sym, root, 3.0, files) {
                if seen.insert((ev.path.clone(), ev.start_line)) {
                    out.push(ev);
                }
            }
        }
    }

    // 2. Modules the claim grounded to: their top-level public symbols.
    for m in &prov.modules {
        for sym in index
            .symbols
            .iter()
            .filter(|s| &s.module == m && s.visibility == Visibility::Public)
            .take(k)
        {
            if let Some(ev) = symbol_evidence(sym, root, 2.0, files) {
                if seen.insert((ev.path.clone(), ev.start_line)) {
                    out.push(ev);
                }
            }
        }
    }

    // 3. Lexical fallback only when grounding produced nothing.
    if out.is_empty() {
        for (idx, score) in lexicon.top(claim_text, k) {
            if let Some(sym) = index.symbols.get(idx) {
                if let Some(ev) = symbol_evidence(sym, root, score, files) {
                    if seen.insert((ev.path.clone(), ev.start_line)) {
                        out.push(ev);
                    }
                }
            }
        }
    }

    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(k);
    out
}

/// Read a symbol's body via its `body_span`, capped to [`MAX_BODY_LINES`].
fn symbol_evidence(
    sym: &crate::code::symbol::Symbol,
    root: &Path,
    score: f32,
    files: &mut FileCache,
) -> Option<Evidence> {
    let span = &sym.body_span;
    if span.path.is_empty() || span.start_line == 0 || span.end_line < span.start_line {
        return None;
    }
    let lines = files
        .entry(span.path.clone())
        .or_insert_with(|| {
            std::fs::read_to_string(root.join(&span.path))
                .ok()
                .map(|c| c.lines().map(str::to_string).collect())
        })
        .as_ref()?;

    let s = span.start_line.saturating_sub(1);
    let e = span.end_line.min(lines.len());
    if s >= e {
        return None;
    }
    let end = e.min(s + MAX_BODY_LINES);
    let text = lines[s..end].join("\n");
    if text.trim().is_empty() {
        return None;
    }
    Some(Evidence {
        text,
        path: span.path.clone(),
        start_line: span.start_line,
        score,
    })
}

// ---- lexical fallback (idf-weighted symbol-table match) --------------------

/// Pre-tokenized, idf-weighted view of the public symbol table for cheap lexical
/// retrieval. Built once per run; scoring a claim is a set intersection.
pub struct Lexicon {
    /// (symbol index in `index.symbols`, its name tokens, its full tokens).
    entries: Vec<(usize, Vec<String>, Vec<String>)>,
    /// token -> inverse document frequency.
    idf: HashMap<String, f32>,
}

impl Lexicon {
    /// Build from the index's public symbols (docs describe public API; private
    /// helpers are noise and bloat the table).
    pub fn build(index: &CodeIndex) -> Lexicon {
        let mut entries = Vec::new();
        let mut df: HashMap<String, usize> = HashMap::new();
        for (i, s) in index.symbols.iter().enumerate() {
            if s.visibility != Visibility::Public {
                continue;
            }
            let name_toks = tokens(&s.name);
            let mut full: Vec<String> = name_toks.clone();
            full.extend(tokens(&s.qualified_name));
            full.extend(tokens(&s.module));
            if let Some(sig) = &s.signature {
                full.extend(tokens(sig));
            }
            if let Some(doc) = &s.doc {
                full.extend(tokens(doc));
            }
            full.sort();
            full.dedup();
            for t in &full {
                *df.entry(t.clone()).or_default() += 1;
            }
            entries.push((i, name_toks, full));
        }
        let n = entries.len().max(1) as f32;
        let idf = df
            .into_iter()
            .map(|(t, d)| (t, (1.0 + n / (1.0 + d as f32)).ln()))
            .collect();
        Lexicon { entries, idf }
    }

    /// Top-`k` (symbol index, score) for a claim, idf-weighted, name matches
    /// boosted. Empty when nothing clears [`MIN_LEXICAL_SCORE`].
    fn top(&self, claim: &str, k: usize) -> Vec<(usize, f32)> {
        let q: std::collections::HashSet<String> = tokens(claim).into_iter().collect();
        if q.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, f32)> = Vec::new();
        for (idx, name_toks, full) in &self.entries {
            let mut score = 0.0f32;
            for t in full {
                if q.contains(t) {
                    let w = self.idf.get(t).copied().unwrap_or(1.0);
                    let boost = if name_toks.contains(t) { 2.0 } else { 1.0 };
                    score += w * boost;
                }
            }
            if score >= MIN_LEXICAL_SCORE {
                scored.push((*idx, score));
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored
    }
}

/// Split text into lowercased subword tokens: break on non-alphanumerics, then
/// on camelCase and digit boundaries, drop very short tokens and stopwords.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in s.split(|c: char| !c.is_alphanumeric()) {
        split_subwords(raw, &mut out);
    }
    out.retain(|t| t.len() >= 3 && !STOPWORDS.contains(&t.as_str()));
    out
}

/// Split a single `[A-Za-z0-9]+` run on camelCase / snake boundaries.
fn split_subwords(raw: &str, out: &mut Vec<String>) {
    if raw.is_empty() {
        return;
    }
    let chars: Vec<char> = raw.chars().collect();
    let mut start = 0;
    for i in 1..chars.len() {
        let prev = chars[i - 1];
        let cur = chars[i];
        let next_lower = chars.get(i + 1).map(|c| c.is_lowercase()).unwrap_or(false);
        let boundary = (prev.is_lowercase() && cur.is_uppercase()) // camelCase
            || (prev.is_uppercase() && cur.is_uppercase() && next_lower) // JSONSchema -> JSON|Schema
            || (prev.is_alphabetic() && cur.is_ascii_digit())
            || (prev.is_ascii_digit() && cur.is_alphabetic());
        if boundary {
            out.push(chars[start..i].iter().collect::<String>().to_lowercase());
            start = i;
        }
    }
    out.push(chars[start..].iter().collect::<String>().to_lowercase());
    // Keep the whole run too (so `model_construct` matches an exact `model_construct`).
    let whole = raw.to_lowercase();
    if !out.last().map(|l| l == &whole).unwrap_or(false) {
        out.push(whole);
    }
}

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "when", "then", "are", "was", "has", "have",
    "not", "but", "all", "any", "can", "you", "use", "used", "uses", "via", "per", "its", "from",
    "into", "only", "must", "may", "should", "would", "will", "does", "doc", "docs", "code",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_split_camel_and_snake() {
        // `_` is split by the outer non-alnum pass, so `model_construct` yields
        // its subwords (which still align with a same-named symbol's subwords).
        let t = tokens("`model_construct` and `JSONSchema` with extraData2");
        assert!(t.contains(&"model".to_string()));
        assert!(t.contains(&"construct".to_string()));
        // Acronym boundary: JSONSchema -> json + schema.
        assert!(t.contains(&"json".to_string()));
        assert!(t.contains(&"schema".to_string()));
        assert!(t.contains(&"extra".to_string()));
        assert!(t.contains(&"data".to_string()));
    }

    #[test]
    fn stopwords_dropped() {
        let t = tokens("the cache invalidates on write");
        assert!(!t.contains(&"the".to_string()));
        assert!(t.contains(&"cache".to_string()));
        assert!(t.contains(&"invalidates".to_string()));
    }
}
