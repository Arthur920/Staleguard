//! Layer 0 drift pipeline: lineage, carry-forward, behavioral-fact drift flag,
//! and the alignment score. Runs *after* the deterministic checks have produced
//! their claims (each a [`Finding`], `Supported` included), enriching them with
//! git + the committed ledger.
//!
//! Degrades safely: with no git or no baseline every claim is treated as dirty
//! (a full scan), so the layer never suppresses a real finding.

pub mod coupling;
pub mod ledger;

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::claim::{claim_id, fnv1a, Provenance};
use crate::code::facts;
use crate::code::symbol::Facts;
use crate::code::CodeIndex;
use crate::findings::{Finding, Verdict};
use crate::git;

use ledger::{ClaimRecord, Ledger, Score};

/// Severity weights for the alignment score. A claim contributes `total` to the
/// denominator and `credit` to the numerator; `Unverifiable` is excluded.
/// Tunable in one place.
fn weight(v: Verdict) -> (f64, f64) {
    match v {
        Verdict::Supported => (1.0, 1.0),
        Verdict::Contradicted => (0.0, 3.0),
        Verdict::Stale => (0.0, 2.0),
        Verdict::Undocumented => (0.0, 1.0),
        Verdict::Unverifiable => (0.0, 0.0),
    }
}

/// Score regression smaller than this is treated as noise.
const TOLERANCE: f64 = 1e-9;

/// How the drift run was parameterized.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// Explicit diff base; defaults to the ledger's last commit.
    pub diff_ref: Option<String>,
    /// Persist the updated ledger + score under `.staleguard/`.
    pub write_ledger: bool,
    /// Fail (return a regression) if the repo score dropped below the committed
    /// base score.
    pub fail_on_regression: bool,
}

/// Result of a drift run.
pub struct Outcome {
    /// Reportable findings: the checks' problems plus any behavioral-drift flags.
    pub findings: Vec<Finding>,
    pub score: Score,
    /// `Some((base, head))` when `fail_on_regression` tripped.
    pub regression: Option<(f64, f64)>,
    /// Claims unchanged since the base (carried forward rather than re-derived).
    pub carried_forward: usize,
    pub total_claims: usize,
}

/// Run the drift pipeline over all claims produced this run.
pub fn run(claims: Vec<Finding>, index: &CodeIndex, root: &Path, opts: &Options) -> Outcome {
    let ledger = Ledger::load(root);
    let head = git::head_sha(root).unwrap_or_default();

    // The diff base: explicit `--diff`, else the ledger's last commit.
    let base = opts
        .diff_ref
        .clone()
        .or_else(|| ledger.baseline_commit().map(str::to_string));

    // The changed-symbol set, or `None` when we can't diff → treat all as dirty.
    let changed: Option<HashSet<String>> = if git::is_repo(root) {
        base.as_ref()
            .map(|b| git::changed_symbols(index, &git::changed_lines(root, b)))
    } else {
        None
    };

    let mut reportable = Vec::new();
    let mut records: BTreeMap<String, ClaimRecord> = BTreeMap::new();
    let mut scored: Vec<(Verdict, Vec<String>)> = Vec::new();
    let mut carried_forward = 0usize;
    let total_claims = claims.len();

    for f in claims {
        let id = claim_id(&f.doc_path, &f.claim);
        let dirty = match &changed {
            None => true,
            Some(set) => is_dirty(&f.provenance, set) || ledger.get(&id).is_none(),
        };
        if !dirty {
            carried_forward += 1;
        }

        let fhash = claim_facts_hash(&f.provenance, index);

        // Behavioral-fact drift flag: a dirty claim whose anchored code changed
        // its fingerprint since the baseline. Only meaningful with a prior record
        // and a real (non-empty) fingerprint on both sides.
        if dirty {
            if let Some(rec) = ledger.get(&id) {
                if fhash != 0 && rec.facts_hash != 0 && rec.facts_hash != fhash {
                    reportable.push(drift_flag(&f, rec.facts_hash, fhash, &rec.commit));
                }
            }
        }

        let mods = claim_modules(&f.provenance, index);
        scored.push((f.verdict, mods));

        records.insert(
            id.clone(),
            ClaimRecord {
                id,
                doc_ref: f.doc_path.clone(),
                provenance: f.provenance.clone(),
                facts: claim_facts(&f.provenance, index),
                facts_hash: fhash,
                verdict: f.verdict,
                commit: head.clone(),
            },
        );

        if f.verdict.is_reportable() {
            reportable.push(f);
        }
    }

    let score = compute_score(&scored, &head);

    // Compare against the committed base score *before* overwriting it.
    let regression = if opts.fail_on_regression {
        Score::load(root).and_then(|base| {
            (score.repo + TOLERANCE < base.repo).then_some((base.repo, score.repo))
        })
    } else {
        None
    };

    if opts.write_ledger {
        let new_ledger = Ledger {
            version: Ledger::VERSION,
            claims: records,
        };
        let _ = new_ledger.save(root);
        let _ = score.save(root);
    }

    Outcome {
        findings: reportable,
        score,
        regression,
        carried_forward,
        total_claims,
    }
}

/// A claim is dirty if any anchor changed, or it has no anchor at all (an
/// ungrounded claim can never be safely carried forward).
fn is_dirty(prov: &Provenance, changed: &HashSet<String>) -> bool {
    if prov.is_empty() {
        return true;
    }
    prov.anchors().any(|a| changed.contains(a)) || prov.paths.iter().any(|p| changed.contains(p))
}

/// Combined fingerprint of the symbols a claim is anchored to. `0` when the
/// claim resolves to no symbol (module/path-anchored claims carry no fingerprint
/// — lineage and scoring still apply, but the drift flag never fires).
fn claim_facts_hash(prov: &Provenance, index: &CodeIndex) -> u64 {
    let mut hashes: Vec<u64> = resolved_symbols(prov, index)
        .map(|s| facts::facts_hash(&s.facts))
        .collect();
    if hashes.is_empty() {
        return 0;
    }
    hashes.sort_unstable();
    hashes.dedup();
    fnv1a(
        &hashes
            .iter()
            .map(|h| h.to_string())
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// A merged view of the anchored symbols' facts, stored for ledger transparency.
fn claim_facts(prov: &Provenance, index: &CodeIndex) -> Facts {
    let mut merged = Facts::default();
    for s in resolved_symbols(prov, index) {
        merged.constants.extend(s.facts.constants.iter().cloned());
        merged.predicates.extend(s.facts.predicates.iter().cloned());
        if merged.signature.is_none() {
            merged.signature = s.facts.signature.clone();
        }
        if merged.return_shape.is_none() {
            merged.return_shape = s.facts.return_shape.clone();
        }
    }
    merged.constants.sort();
    merged.constants.dedup();
    merged.predicates.sort();
    merged.predicates.dedup();
    merged
}

/// Symbols an anchor points at — matched by qualified name or bare name.
fn resolved_symbols<'a>(
    prov: &'a Provenance,
    index: &'a CodeIndex,
) -> impl Iterator<Item = &'a crate::code::symbol::Symbol> {
    index
        .symbols
        .iter()
        .filter(move |s| prov.symbols.contains(&s.qualified_name) || prov.symbols.contains(&s.name))
}

/// Modules a claim is scored under: its module anchors, the modules of any
/// symbol anchors, and — for path anchors — the module that owns the file
/// (resolved via the index), falling back to the path itself so a doc-referenced
/// file still buckets to *something* meaningful rather than `(unscoped)`.
fn claim_modules(prov: &Provenance, index: &CodeIndex) -> Vec<String> {
    let mut mods: Vec<String> = prov.modules.clone();
    for s in resolved_symbols(prov, index) {
        mods.push(s.module.clone());
    }
    for p in &prov.paths {
        match module_of_path(p, index) {
            Some(m) => mods.push(m),
            None => mods.push(p.clone()),
        }
    }
    mods.sort();
    mods.dedup();
    mods
}

/// The module owning a file path, found via any symbol defined in that file.
/// Matches an exact repo-relative path or a trailing-segment suffix (doc claims
/// often name only `foo.rs`, not `src/foo.rs`). `None` for non-code/unknown
/// files (e.g. `CLAUDE.md`).
fn module_of_path(path: &str, index: &CodeIndex) -> Option<String> {
    let suffix = format!("/{path}");
    index
        .symbols
        .iter()
        .find(|s| s.span.path == path || s.span.path.ends_with(&suffix))
        .map(|s| s.module.clone())
}

fn drift_flag(f: &Finding, old: u64, new: u64, since: &str) -> Finding {
    let since = if since.is_empty() {
        "the baseline".to_string()
    } else {
        format!("`{}`", &since[..since.len().min(12)])
    };
    Finding::problem(
        Verdict::Unverifiable,
        f.claim.clone(),
        f.doc_path.clone(),
        format!(
            "Behavioral drift: code behind this claim changed (fingerprint {old:x} -> {new:x}) since {since}; re-verify."
        ),
    )
    .anchored(f.provenance.clone())
}

const UNSCOPED: &str = "(unscoped)";

fn compute_score(scored: &[(Verdict, Vec<String>)], head: &str) -> Score {
    let mut repo_credit = 0.0;
    let mut repo_total = 0.0;
    // module -> (credit, total)
    let mut per: BTreeMap<String, (f64, f64)> = BTreeMap::new();

    for (verdict, mods) in scored {
        let (credit, total) = weight(*verdict);
        repo_credit += credit;
        repo_total += total;
        if total == 0.0 {
            continue; // excluded verdicts don't affect any bucket
        }
        let keys: Vec<&str> = if mods.is_empty() {
            vec![UNSCOPED]
        } else {
            mods.iter().map(String::as_str).collect()
        };
        for k in keys {
            let e = per.entry(k.to_string()).or_insert((0.0, 0.0));
            e.0 += credit;
            e.1 += total;
        }
    }

    let ratio = |c: f64, t: f64| if t > 0.0 { c / t } else { 1.0 };
    Score {
        repo: ratio(repo_credit, repo_total),
        per_module: per
            .into_iter()
            .map(|(k, (c, t))| (k, ratio(c, t)))
            .collect(),
        commit: head.to_string(),
    }
}

#[cfg(test)]
mod tests;
