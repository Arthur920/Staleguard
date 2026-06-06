//! Claim identity and provenance — the substrate the drift ledger (Layer 0)
//! carries across runs.
//!
//! A *claim* is a doc assertion under test; in this codebase a [`Finding`] with
//! its [`Provenance`] *is* that claim (a `Supported` finding is a claim that
//! verified). Two pieces are needed beyond what a finding already carries:
//!
//! - **Provenance** — which code the claim is anchored to (symbols / modules /
//!   files). Lineage invalidation walks these: a claim is dirty when any of its
//!   anchors changed. Symbols are the preferred anchor (they survive moves);
//!   modules and paths are coarser fallbacks.
//! - **A stable id** — `fnv1a(doc_path + normalized claim text)`. The ledger is
//!   committed, so the hash must be stable across machines *and* toolchain
//!   versions; `std::hash::DefaultHasher` guarantees neither, hence the inline
//!   FNV-1a here.
//!
//! [`Finding`]: crate::findings::Finding

use serde::{Deserialize, Serialize};

/// What a claim is anchored to in the code. Empty means "ungrounded" — such a
/// claim is always treated as dirty (it can never be carried forward).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// `qualified_name`s of the symbols the claim points at (preferred anchor).
    pub symbols: Vec<String>,
    /// module paths the claim points at (coarser fallback).
    pub modules: Vec<String>,
    /// repo-relative file paths (path/command claims).
    pub paths: Vec<String>,
}

impl Provenance {
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty() && self.modules.is_empty() && self.paths.is_empty()
    }

    /// A claim anchored to a single symbol.
    pub fn symbol(name: impl Into<String>) -> Provenance {
        Provenance {
            symbols: vec![name.into()],
            ..Default::default()
        }
    }

    /// A claim anchored to one or more modules.
    pub fn modules(mods: impl IntoIterator<Item = String>) -> Provenance {
        Provenance {
            modules: mods.into_iter().collect(),
            ..Default::default()
        }
    }

    /// A claim anchored to a single file path.
    pub fn path(p: impl Into<String>) -> Provenance {
        Provenance {
            paths: vec![p.into()],
            ..Default::default()
        }
    }

    /// Every anchor key, symbols and modules together — what lineage tests
    /// against the changed-symbol set. (Paths are matched separately, by file.)
    pub fn anchors(&self) -> impl Iterator<Item = &String> {
        self.symbols.iter().chain(self.modules.iter())
    }
}

/// FNV-1a (64-bit). Deterministic across machines and Rust versions — required
/// for the committed ledger.
pub fn fnv1a(s: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Stable id for a claim: `fnv1a(doc_path + "\0" + normalized claim text)`,
/// rendered as zero-padded hex. Normalization collapses runs of whitespace and
/// trims, so cosmetic reflowing of a doc line keeps the same id.
pub fn claim_id(doc_path: &str, claim_text: &str) -> String {
    let normalized = normalize(claim_text);
    format!("{:016x}", fnv1a(&format!("{doc_path}\0{normalized}")))
}

/// Collapse internal whitespace runs to a single space and trim the ends.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_is_deterministic_and_distinct() {
        assert_eq!(fnv1a("retries = 3"), fnv1a("retries = 3"));
        assert_ne!(fnv1a("retries = 3"), fnv1a("retries = 5"));
        // Known FNV-1a anchor: empty string hashes to the offset basis.
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn claim_id_is_normalization_stable() {
        assert_eq!(
            claim_id("a.md", "runs `npm run build`"),
            claim_id("a.md", "runs   `npm run build`  ")
        );
        // doc_path participates in identity.
        assert_ne!(
            claim_id("a.md", "runs `x`"),
            claim_id("b.md", "runs `x`")
        );
    }

    #[test]
    fn provenance_anchors_chains_symbols_and_modules() {
        let p = Provenance {
            symbols: vec!["a::b".into()],
            modules: vec!["src/a".into()],
            paths: vec!["README.md".into()],
        };
        let got: Vec<&String> = p.anchors().collect();
        assert_eq!(got, vec!["a::b", "src/a"]);
        assert!(!p.is_empty());
    }
}
