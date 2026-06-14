//! The committed claim ledger and alignment-score artifacts (`.staleguard/`).
//!
//! Both are facts-only JSON (no embeddings), so they are commit-safe and
//! merge-friendly. The ledger carries one [`ClaimRecord`] per claim across runs;
//! the [`Score`] is the single scalar CI compares base-vs-head. Loads return an
//! empty default when the file is absent (cold start) or unparseable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::claim::Provenance;
use crate::code::symbol::Facts;
use crate::findings::Verdict;

/// Directory holding the committed drift artifacts.
pub const DIR: &str = ".staleguard";

fn ledger_path(root: &Path) -> PathBuf {
    root.join(DIR).join("ledger.json")
}

fn score_path(root: &Path) -> PathBuf {
    root.join(DIR).join("score.json")
}

/// One claim, carried across runs. Keyed in the ledger by its stable id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRecord {
    pub id: String,
    pub doc_ref: String,
    #[serde(default)]
    pub provenance: Provenance,
    #[serde(default)]
    pub facts: Facts,
    pub facts_hash: u64,
    pub verdict: Verdict,
    /// HEAD sha when this record was last verified.
    pub commit: String,
}

/// The full claim ledger.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub claims: BTreeMap<String, ClaimRecord>,
}

impl Ledger {
    pub const VERSION: u32 = 1;

    /// Load the committed ledger, or an empty one on cold start / parse error.
    pub fn load(root: &Path) -> Ledger {
        std::fs::read_to_string(ledger_path(root))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// The most recent commit any record was verified at — the implicit diff
    /// base when `--diff` isn't given. `None` for an empty ledger.
    pub fn baseline_commit(&self) -> Option<&str> {
        self.claims.values().map(|c| c.commit.as_str()).next()
    }

    pub fn get(&self, id: &str) -> Option<&ClaimRecord> {
        self.claims.get(id)
    }

    /// Persist the ledger under `.staleguard/` (created on demand).
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let dir = root.join(DIR);
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self).unwrap();
        std::fs::write(ledger_path(root), json)
    }
}

/// Per-module and repo-wide alignment score (severity-weighted supported/total).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Score {
    pub repo: f64,
    #[serde(default)]
    pub per_module: BTreeMap<String, f64>,
    #[serde(default)]
    pub commit: String,
}

impl Score {
    /// Load the committed score (the CI base), if any.
    pub fn load(root: &Path) -> Option<Score> {
        let t = std::fs::read_to_string(score_path(root)).ok()?;
        serde_json::from_str(&t).ok()
    }

    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let dir = root.join(DIR);
        std::fs::create_dir_all(&dir)?;
        let json = serde_json::to_string_pretty(self).unwrap();
        std::fs::write(score_path(root), json)
    }
}
