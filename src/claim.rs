//! Claim provenance: which code a doc claim is anchored to (symbols / modules /
//! files). Carried on each finding and emitted in JSON output.

use serde::{Deserialize, Serialize};

/// What a claim is anchored to in the code. Empty means "ungrounded".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// `qualified_name`s of the symbols the claim points at.
    pub symbols: Vec<String>,
    /// module paths the claim points at.
    pub modules: Vec<String>,
    /// repo-relative file paths (path/command claims).
    pub paths: Vec<String>,
}

impl Provenance {
    /// A claim anchored to a single symbol.
    pub fn symbol(name: impl Into<String>) -> Provenance {
        Provenance {
            symbols: vec![name.into()],
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
}
