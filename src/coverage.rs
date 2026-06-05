//! Coverage gaps: the code → doc traversal (Layer 1, deterministic).
//!
//! The inverse of `verify` — instead of checking a doc claim against the code,
//! it starts from the code's public surface and asks whether any doc describes
//! it. A public symbol whose name appears in no doc is an `undocumented` gap.
//!
//! v1 scope (see `docs/coverage-gaps.md`): public surface only; "documented"
//! means the name appears as a token anywhere in any doc (loose presence, fewest
//! false positives); module fan-in *ranks* gaps but never suppresses them.
//! Dead-code disambiguation, `term-drift`, and `under-documented` are deferred.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::code::symbol::{Symbol, SymbolKind, Visibility};
use crate::code::CodeIndex;
use crate::findings::{Finding, Verdict};

/// Identifier-like tokens (len ≥ 2). Underscores are part of a token, so a name
/// like `check_paths` matches as a whole; runs over the whole doc, so prose and
/// `backtick` spans are both covered.
fn ident_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_]+").unwrap())
}

/// Extract and print code → doc coverage gaps for a repo.
pub fn run(repo_root: &Path) -> Vec<Finding> {
    let index = CodeIndex::build(repo_root);
    let terms = build_doc_terms(repo_root);
    find_gaps(&index, &terms)
}

/// Every identifier-like token mentioned across all markdown docs in the repo.
fn build_doc_terms(repo_root: &Path) -> HashSet<String> {
    let mut terms = HashSet::new();
    for doc in crate::collect_docs(repo_root) {
        if let Ok(text) = std::fs::read_to_string(&doc) {
            for m in ident_re().find_iter(&text) {
                terms.insert(m.as_str().to_string());
            }
        }
    }
    terms
}

/// Public symbols whose name is mentioned in no doc, ranked by symbol fan-in
/// (number of distinct internal callers; highest risk first), then by location
/// for stable output.
fn find_gaps(index: &CodeIndex, terms: &HashSet<String>) -> Vec<Finding> {
    let mut gaps: Vec<(usize, &Symbol)> = index
        .symbols
        .iter()
        .filter(|s| s.visibility == Visibility::Public)
        .filter(|s| !terms.contains(&s.name))
        .map(|s| (index.symbol_fan_in(&s.qualified_name), s))
        .collect();

    gaps.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.span.path.cmp(&b.1.span.path))
            .then_with(|| a.1.span.start_line.cmp(&b.1.span.start_line))
    });

    gaps.into_iter()
        .map(|(fan_in, s)| finding_for(s, fan_in))
        .collect()
}

fn finding_for(s: &Symbol, fan_in: usize) -> Finding {
    let kind = kind_label(&s.kind);
    // Soft reachability hint: a zero-caller public symbol is either dead code or
    // a true entry point. We don't suppress it — just flag the signal.
    let reach = if fan_in == 0 {
        "no internal callers".to_string()
    } else {
        format!("fan-in {fan_in}")
    };
    Finding {
        verdict: Verdict::Undocumented,
        claim: format!("public {} `{}` has no doc reference", kind.to_lowercase(), s.name),
        doc_path: format!("{}:{}", s.span.path, s.span.start_line),
        detail: format!(
            "{} `{}` ({}) is documented nowhere; {}.",
            kind, s.name, s.module, reach
        ),
        layer: 1,
        code_refs: Vec::new(),
    }
}

fn kind_label(kind: &SymbolKind) -> String {
    match kind {
        SymbolKind::Other(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::symbol::{Facts, RefEdge, Span};

    fn sym(name: &str, vis: Visibility, module: &str) -> Symbol {
        Symbol {
            qualified_name: format!("{module}::{name}"),
            name: name.to_string(),
            kind: SymbolKind::Function,
            visibility: vis,
            module: module.to_string(),
            span: Span {
                path: format!("{module}.rs"),
                start_line: 1,
                end_line: 1,
            },
            signature: None,
            doc: None,
            facts: Facts::default(),
        }
    }

    fn terms(words: &[&str]) -> HashSet<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn public_symbol_absent_from_docs_is_flagged() {
        let index = CodeIndex {
            symbols: vec![sym("frobnicate", Visibility::Public, "m")],
            edges: vec![],
            module_edges: vec![],
            ref_edges: vec![],
        };
        let gaps = find_gaps(&index, &terms(&["something", "else"]));
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].verdict, Verdict::Undocumented);
        assert!(gaps[0].detail.contains("frobnicate"));
        // zero callers -> the soft reachability hint.
        assert!(gaps[0].detail.contains("no internal callers"));
    }

    #[test]
    fn documented_symbol_not_flagged() {
        let index = CodeIndex {
            symbols: vec![sym("frobnicate", Visibility::Public, "m")],
            edges: vec![],
            module_edges: vec![],
            ref_edges: vec![],
        };
        assert!(find_gaps(&index, &terms(&["frobnicate"])).is_empty());
    }

    #[test]
    fn private_and_internal_symbols_not_flagged() {
        let index = CodeIndex {
            symbols: vec![
                sym("helper", Visibility::Private, "m"),
                sym("internal", Visibility::Internal, "m"),
            ],
            edges: vec![],
            module_edges: vec![],
            ref_edges: vec![],
        };
        assert!(find_gaps(&index, &HashSet::new()).is_empty());
    }

    #[test]
    fn higher_fan_in_ranks_first() {
        // hot_fn has two distinct callers; cold_fn has none.
        let index = CodeIndex {
            symbols: vec![
                sym("cold_fn", Visibility::Public, "cold"),
                sym("hot_fn", Visibility::Public, "hot"),
            ],
            edges: vec![],
            module_edges: vec![],
            ref_edges: vec![
                RefEdge {
                    from_symbol: "cold::cold_fn".into(),
                    to_symbol: "hot::hot_fn".into(),
                },
                RefEdge {
                    from_symbol: "other::caller".into(),
                    to_symbol: "hot::hot_fn".into(),
                },
            ],
        };
        let gaps = find_gaps(&index, &HashSet::new());
        assert_eq!(gaps.len(), 2);
        assert!(gaps[0].detail.contains("hot_fn"));
        assert!(gaps[0].detail.contains("fan-in 2"));
        assert!(gaps[1].detail.contains("cold_fn"));
        assert!(gaps[1].detail.contains("no internal callers"));
    }

    #[test]
    fn run_flags_only_undocumented_public_symbol() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("shlomes-cov-{nanos}"));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/lib.rs"),
            "pub fn documented_fn() {}\npub fn hidden_fn() {}\n",
        )
        .unwrap();
        fs::write(dir.join("README.md"), "We expose `documented_fn` for callers.\n").unwrap();

        let findings = run(&dir);
        assert!(findings.iter().any(|f| f.detail.contains("hidden_fn")));
        assert!(!findings.iter().any(|f| f.detail.contains("documented_fn")));
    }
}
