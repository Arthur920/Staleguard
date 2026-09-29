//! Language-aware code extractor: turns source into symbols + dependency edges.
//!
//! The shared substrate for coverage-gaps, diagram edge-diff, architecture
//! rules, and drift provenance/fingerprints. Default build (no `ml` feature).

mod extract;
pub mod facts;
pub mod lang;
mod resolve;
pub mod symbol;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;
use serde::Serialize;

use extract::RawRef;
use lang::Language;
use symbol::{DepEdge, RefEdge, Symbol};

/// All symbols, module dependency edges, and symbol-level reference edges
/// extracted from a repo.
#[derive(Debug, Default, Serialize)]
pub struct CodeIndex {
    pub symbols: Vec<Symbol>,
    /// Raw dependency edges: `from_module` is a repo module path, `to_module` is
    /// the import *as written* (`crate::x`, `./mod`, `os`).
    pub edges: Vec<DepEdge>,
    /// Resolved internal module graph: both endpoints are repo module paths, and
    /// only edges whose target resolves to a real module are kept. The clean
    /// substrate architecture-rule checks run against.
    pub module_edges: Vec<DepEdge>,
    /// Symbol reference graph keyed by target: `ref_callers[to]` is the distinct
    /// set of `from` symbols that reference `to`. Keying by target makes
    /// [`Self::symbol_fan_in`] an O(1) lookup. Coverage calls it once per symbol,
    /// so an O(edges) scan would cost O(symbols × edges), which hangs on large
    /// repos (litellm).
    /// Each edge also stores one interned `Arc<str>` caller (under its shared
    /// target key) instead of two cloned names. Serializes back to the original
    /// flat `[{from_symbol, to_symbol}]` array under the key `ref_edges`, so the
    /// `index` dump shape is unchanged.
    #[serde(rename = "ref_edges", serialize_with = "serialize_ref_callers")]
    pub ref_callers: HashMap<Arc<str>, Vec<Arc<str>>>,
}

/// Flatten the target-keyed caller map back into the historical
/// `[{from_symbol, to_symbol}]` array, sorted for a deterministic dump.
fn serialize_ref_callers<S: serde::Serializer>(
    callers: &HashMap<Arc<str>, Vec<Arc<str>>>,
    s: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut flat: Vec<(&str, &str)> = callers
        .iter()
        .flat_map(|(to, froms)| froms.iter().map(move |from| (from.as_ref(), to.as_ref())))
        .collect();
    flat.sort_unstable();
    let mut seq = s.serialize_seq(Some(flat.len()))?;
    for (from, to) in flat {
        seq.serialize_element(&RefEdge {
            from_symbol: Arc::from(from),
            to_symbol: Arc::from(to),
        })?;
    }
    seq.end()
}

/// Build the target-keyed caller map from flat edges, deduping callers per
/// target. The inverse of the flat serialization above; used by tests that
/// construct a [`CodeIndex`] from explicit edges.
#[cfg(test)]
pub fn ref_callers_from(
    edges: impl IntoIterator<Item = RefEdge>,
) -> HashMap<Arc<str>, Vec<Arc<str>>> {
    let mut map: HashMap<Arc<str>, Vec<Arc<str>>> = HashMap::new();
    for e in edges {
        let froms = map.entry(e.to_symbol).or_default();
        if !froms.contains(&e.from_symbol) {
            froms.push(e.from_symbol);
        }
    }
    map
}

impl CodeIndex {
    /// Walk every code file under `repo_root` and extract symbols + edges, then
    /// resolve raw references into symbol-level reference edges across files.
    pub fn build(repo_root: &Path) -> CodeIndex {
        // Parse files in parallel; each `extract_file` owns its tree-sitter
        // parser, so there's no shared state. `collect` into an ordered Vec keeps
        // the merge deterministic (stable symbol order = stable output).
        let files = lang::code_files(repo_root);
        let per_file: Vec<_> = files
            .par_iter()
            .map(|file| extract::extract_file(file, repo_root))
            .collect();

        // Merge symbols/edges into flat Vecs, but keep each file's raw refs in
        // their own already-allocated Vec rather than concatenating them; the raw
        // (pre-resolution) ref set is the largest collection on large repos, and a
        // single flattened copy would double its peak footprint. `resolve_refs`
        // only streams them once, so we hand it a lazy `flatten()` instead.
        let mut symbols = Vec::new();
        let mut edges = Vec::new();
        let mut raw_per_file: Vec<Vec<RawRef>> = Vec::with_capacity(per_file.len());
        for (s, e, r) in per_file {
            symbols.extend(s);
            edges.extend(e);
            raw_per_file.push(r);
        }
        let ref_callers = resolve_refs(&symbols, raw_per_file.into_iter().flatten());
        // Every parsed file is a module, even one with no symbols (a barrel
        // `index.ts` of re-exports), so imports of it still resolve.
        let file_langs: HashMap<String, Language> = files
            .iter()
            .filter_map(|f| Some((lang::module_path(f, repo_root), Language::from_path(f)?)))
            .collect();
        let tsconfigs = resolve::load_tsconfigs(repo_root);
        let module_edges = resolve_module_edges(&file_langs, &edges, &tsconfigs);
        CodeIndex {
            symbols,
            edges,
            module_edges,
            ref_callers,
        }
    }

    /// Number of distinct symbols that reference `qualified_name`: the
    /// per-symbol risk signal for coverage-gaps, and the basis for the
    /// dead-code-vs-undocumented distinction.
    pub fn symbol_fan_in(&self, qualified_name: &str) -> usize {
        // Callers are deduped per target at construction, so the stored length is
        // already the distinct-caller count, an O(1) lookup.
        self.ref_callers.get(qualified_name).map_or(0, Vec::len)
    }
}

/// Turn raw import edges into a resolved internal module graph: map each
/// `to_module` (the import as written) to a real repo module via the source
/// language's rules, dropping externals, self-edges, and duplicates.
/// `file_langs` maps every parsed file's module path to its language.
fn resolve_module_edges(
    file_langs: &HashMap<String, Language>,
    edges: &[DepEdge],
    tsconfigs: &[resolve::TsConfig],
) -> Vec<DepEdge> {
    let module_set: HashSet<String> = file_langs.keys().cloned().collect();

    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();
    for e in edges {
        let Some(&lang) = file_langs.get(&e.from_module) else {
            continue;
        };
        let Some(to) =
            resolve::resolve_import(&e.to_module, &e.from_module, lang, &module_set, tsconfigs)
        else {
            continue;
        };
        if to == e.from_module {
            continue;
        }
        if seen.insert((e.from_module.clone(), to.clone())) {
            out.push(DepEdge {
                from_module: e.from_module.clone(),
                to_module: to,
            });
        }
    }
    out
}

/// A name defined more than this many times is a common identifier (`new`,
/// `get`, `build`, `next`): name-based resolution would fan every reference out
/// to all of them, an `O(refs × defs)` blow-up that produced hundreds of
/// millions of edges (and OOM) on large repos. Such names carry no signal, so
/// we skip resolving them rather than explode.
const MAX_DEFS_PER_NAME: usize = 32;

/// Resolve raw references (name + enclosing symbol) into symbol-level edges.
/// A reference name is matched to every same-named definition (over-approximate,
/// so it never under-counts callers), except names with more than
/// [`MAX_DEFS_PER_NAME`] definitions, which are dropped as noise. Self-edges and
/// duplicate `(from, to)` pairs are dropped.
///
/// Dedup is done over interned `u32` ids rather than cloned `String` pairs, so
/// the `seen` set costs 8 bytes per pair instead of two heap allocations, and the
/// target-keyed caller map is built in a single pass (no intermediate edge list).
fn resolve_refs(
    symbols: &[Symbol],
    raw_refs: impl IntoIterator<Item = RawRef>,
) -> HashMap<Arc<str>, Vec<Arc<str>>> {
    let mut by_name: HashMap<&str, Vec<&str>> = HashMap::new();
    for s in symbols {
        by_name
            .entry(s.name.as_str())
            .or_default()
            .push(s.qualified_name.as_str());
    }

    // Intern qualified names to small ids *and* a shared `Arc<str>` per distinct
    // endpoint (linear in symbols), so each emitted edge reuses one allocation
    // rather than cloning two long names. `pool[id]` is the interned name for id.
    // `intern` is a free fn (not a closure capturing `pool`), so its borrow of
    // `pool` is released at each return, letting us read `pool[id]` in the same
    // loop and build the caller map in one pass, with no intermediate edge list.
    fn intern(ids: &mut HashMap<Arc<str>, u32>, pool: &mut Vec<Arc<str>>, s: &str) -> u32 {
        if let Some(&i) = ids.get(s) {
            return i;
        }
        let i = pool.len() as u32;
        let arc: Arc<str> = Arc::from(s);
        pool.push(arc.clone());
        ids.insert(arc, i);
        i
    }

    let mut ids: HashMap<Arc<str>, u32> = HashMap::new();
    let mut pool: Vec<Arc<str>> = Vec::new();
    // `seen` dedups `(from, to)` over 8-byte id pairs (no string allocs); callers
    // are grouped by target as we go. A hot callee's many callers each cost one
    // `Arc` clone (a refcount bump), not a fresh copy of its long name.
    let mut seen: HashSet<(u32, u32)> = HashSet::new();
    let mut callers: HashMap<Arc<str>, Vec<Arc<str>>> = HashMap::new();
    for r in raw_refs {
        let Some(targets) = by_name.get(r.name.as_str()) else {
            continue;
        };
        if targets.len() > MAX_DEFS_PER_NAME {
            continue;
        }
        let from_id = intern(&mut ids, &mut pool, &r.from);
        for &to in targets {
            if to == r.from {
                continue;
            }
            let to_id = intern(&mut ids, &mut pool, to);
            if seen.insert((from_id, to_id)) {
                let to_arc = pool[to_id as usize].clone();
                callers
                    .entry(to_arc)
                    .or_default()
                    .push(pool[from_id as usize].clone());
            }
        }
    }
    callers
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbol::{Facts, Span, SymbolKind, Visibility};

    /// A typical TS repo: dotted file names, a barrel `index.ts` with only
    /// re-exports, a tsconfig path alias, and a re-export. Every import must
    /// land in the module graph.
    #[test]
    fn typescript_imports_resolve_end_to_end() {
        let dir = std::env::temp_dir().join(format!(
            "staleguard-ts-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let files = [
            (
                "src/app.ts",
                "import { u } from \"./user.service\";\nimport { h } from \"./lib\";\n\
                 import { k } from \"@/lib/keys\";\nexport { z } from \"./ui/z\";\n",
            ),
            ("src/user.service.ts", "export const u = 1;\n"),
            ("src/lib/index.ts", "export * from \"./keys\";\n"),
            ("src/lib/keys.ts", "export const k = 1;\n"),
            ("src/ui/z.ts", "export const z = 1;\n"),
            (
                "tsconfig.json",
                "{ // aliases\n \"compilerOptions\": { \"paths\": { \"@/*\": [\"src/*\"] } } }",
            ),
        ];
        for (path, body) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        let index = CodeIndex::build(&dir);
        let mut got: Vec<(&str, &str)> = index
            .module_edges
            .iter()
            .map(|e| (e.from_module.as_str(), e.to_module.as_str()))
            .collect();
        got.sort();
        assert_eq!(
            got,
            [
                ("src/app", "src/lib/index"),
                ("src/app", "src/lib/keys"),
                ("src/app", "src/ui/z"),
                ("src/app", "src/user.service"),
                ("src/lib/index", "src/lib/keys"),
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sym(name: &str, qualified: &str) -> Symbol {
        Symbol {
            qualified_name: qualified.to_string(),
            name: name.to_string(),
            kind: SymbolKind::Function,
            visibility: Visibility::Public,
            module: "m".to_string(),
            span: Span {
                path: "m.rs".to_string(),
                start_line: 1,
                end_line: 1,
            },
            body_span: Span::zero(),
            signature: None,
            doc: None,
            facts: Facts::default(),
        }
    }

    #[test]
    fn fan_in_counts_distinct_callers() {
        let symbols = vec![
            sym("target", "m::target"),
            sym("a", "m::a"),
            sym("b", "m::b"),
        ];
        // a and b each call target; a calls it twice -> still one distinct caller.
        let raw = vec![
            RawRef {
                from: "m::a".into(),
                name: "target".into(),
            },
            RawRef {
                from: "m::a".into(),
                name: "target".into(),
            },
            RawRef {
                from: "m::b".into(),
                name: "target".into(),
            },
        ];
        let ref_callers = resolve_refs(&symbols, raw);
        let index = CodeIndex {
            symbols,
            edges: vec![],
            module_edges: vec![],
            ref_callers,
        };
        assert_eq!(index.symbol_fan_in("m::target"), 2);
    }

    #[test]
    fn over_approximates_on_name_collision() {
        let symbols = vec![
            sym("run", "a::run"),
            sym("run", "b::run"),
            sym("caller", "c::caller"),
        ];
        let raw = vec![RawRef {
            from: "c::caller".into(),
            name: "run".into(),
        }];
        let callers = resolve_refs(&symbols, raw);
        assert!(callers.contains_key("a::run"));
        assert!(callers.contains_key("b::run"));
    }

    #[test]
    fn drops_self_edges() {
        let symbols = vec![sym("foo", "m::foo")];
        let raw = vec![RawRef {
            from: "m::foo".into(),
            name: "foo".into(),
        }];
        assert!(resolve_refs(&symbols, raw).is_empty());
    }
}
