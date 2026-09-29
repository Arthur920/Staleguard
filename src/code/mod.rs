//! Language-aware code extractor: turns source into symbols.
//!
//! Symbols ground qualified doc references (`entrypoints`) and anchor drift
//! provenance/fingerprints (`drift`).

mod extract;
pub mod facts;
pub mod lang;
pub mod symbol;

use std::path::Path;

use rayon::prelude::*;

use symbol::Symbol;

/// All symbols extracted from a repo.
#[derive(Debug, Default)]
pub struct CodeIndex {
    pub symbols: Vec<Symbol>,
}

impl CodeIndex {
    /// Walk every code file under `repo_root` and extract its symbols.
    pub fn build(repo_root: &Path) -> CodeIndex {
        // Parse files in parallel; each `extract_file` owns its tree-sitter
        // parser, so there's no shared state. `collect` into an ordered Vec keeps
        // the merge deterministic (stable symbol order = stable output).
        let symbols = lang::code_files(repo_root)
            .par_iter()
            .map(|file| extract::extract_file(file, repo_root))
            .collect::<Vec<_>>()
            .into_iter()
            .flatten()
            .collect();
        CodeIndex { symbols }
    }
}
