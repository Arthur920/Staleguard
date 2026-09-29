//! Layer 1 diagram coherence: parse text-based architecture diagrams and
//! set-diff their nodes/edges against the real module dependency graph.
//!
//! Scope: Mermaid only. `graph`/`flowchart` diagrams are set-diffed against the
//! import graph; `classDiagram`s are grounded against real symbols. Other kinds
//! and formats (sequence/ER/state, PlantUML, DOT) were dropped: rare in real
//! docs, and never produced a true finding in the wild audits. No ML: every endpoint is grounded against
//! real modules with [`crate::rules::matches`] / [`crate::rules::grounded`] so we
//! under-report rather than emit false positives.

mod class;
mod ground;
mod mermaid;

use std::collections::HashSet;

use crate::claim::Provenance;
use crate::code::CodeIndex;
use crate::findings::{Finding, Verdict};
use crate::rules::matches;

use ground::{ground_label, module_token_index, resolve, Resolution};

/// A diagram box. `id` is the node key used by edges; `label` is the display
/// text (falls back to `id` when a node is only referenced, never declared).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub id: String,
    pub label: String,
}

/// A drawn connection between two node ids. `directed` distinguishes `A --> B`
/// (an import direction) from `A --- B` (undirected).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub directed: bool,
    /// False when the edge's label marks it as intentionally non-real
    /// (`deprecated`, `TODO`, `planned`): it is still *drawn* (so it satisfies
    /// the missing-arrow check) but asserts no live dependency, so it can't be
    /// a phantom edge.
    pub asserted: bool,
}

/// An edge label that flags the connection as intentionally not (or no longer)
/// real — asserting nothing about the current import graph.
pub(super) fn non_real_label(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    [
        "deprecat", "todo", "planned", "removed", "future", "obsolete",
    ]
    .iter()
    .any(|w| l.contains(w))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagram {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// `"rel/path.md:<block-start-line>"`.
    pub origin: String,
}

impl Diagram {
    /// Display text for a node id: its declared label, else the id itself.
    fn text(&self, id: &str) -> String {
        self.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.label.clone())
            .unwrap_or_else(|| id.to_string())
    }
}

/// A fenced mermaid block pulled from a markdown doc.
struct Source {
    body: String,
    /// 1-based line where the block starts.
    line: usize,
}

/// Every ```` ```mermaid ```` block in a markdown document.
fn sources(markdown: &str) -> Vec<Source> {
    let mut out = Vec::new();
    // (is-mermaid, start-line, body) of the open fence, if any.
    let mut fence: Option<(bool, usize, Vec<&str>)> = None;
    for (i, raw) in markdown.lines().enumerate() {
        let trimmed = raw.trim_start();
        if let Some((is_mermaid, start, body)) = fence.as_mut() {
            if trimmed.starts_with("```") {
                if *is_mermaid {
                    out.push(Source {
                        body: body.join("\n"),
                        line: *start,
                    });
                }
                fence = None;
            } else {
                body.push(raw);
            }
        } else if let Some(rest) = trimmed.strip_prefix("```") {
            let is_mermaid = rest.trim().eq_ignore_ascii_case("mermaid");
            fence = Some((is_mermaid, i + 1, Vec::new()));
        }
    }
    out
}

/// Diagram-coherence findings for one markdown document.
pub fn check(
    markdown: &str,
    doc_path: &str,
    index: &CodeIndex,
    modules: &HashSet<String>,
) -> Vec<Finding> {
    let mut out = Vec::new();
    for src in sources(markdown) {
        let origin = format!("{doc_path}:{}", src.line);
        match mermaid::parse(&src.body, &origin) {
            Some(d) => out.extend(diff(&d, index, modules)),
            None => out.extend(class::check(&src.body, &origin, index)),
        }
    }
    out
}

// ---- the set-diff ---------------------------------------------------------

/// Does a real module edge connect a module matching `from` to one matching
/// `to`? `from`/`to` are diagram labels; `module_edges` endpoints are real paths.
fn real_edge(index: &CodeIndex, from: &str, to: &str) -> bool {
    index
        .module_edges
        .iter()
        .any(|e| matches(&e.from_module, from) && matches(&e.to_module, to))
}

/// Set-diff a parsed diagram against the real import graph. Every comparison is
/// gated by grounding, so external boxes (`User`, `DB`, `Browser`) are ignored.
fn diff(d: &Diagram, index: &CodeIndex, modules: &HashSet<String>) -> Vec<Finding> {
    let mut out = Vec::new();
    let module_tokens = module_token_index(modules);
    let res = |label: &str| resolve(label, modules, &module_tokens);

    // Resolve every node label once (exact or fuzzy).
    let node_res: Vec<(&str, Resolution)> = d
        .nodes
        .iter()
        .map(|n| (n.label.as_str(), res(&n.label)))
        .collect();

    // 1. Phantom edges: drawn, both endpoints name exactly one real module, but
    //    no real import connects them. Exact-unique only: a fuzzily- or ambiguously-
    //    grounded endpoint can't carry an assertion about a specific edge without
    //    risking false positives (the wild audit's conceptual/segment-match arrows).
    for e in &d.edges {
        if !e.asserted {
            continue; // labeled deprecated/planned — asserts no live dependency
        }
        let from = d.text(&e.from);
        let to = d.text(&e.to);
        let (rfrom, rto) = (res(&from), res(&to));
        let (Some(gfrom), Some(gto)) = (rfrom.exact_module(), rto.exact_module()) else {
            continue; // an endpoint is external/undocumented/ambiguous → skip
        };
        let exists = real_edge(index, gfrom, gto) || (!e.directed && real_edge(index, gto, gfrom));
        let prov = Provenance::modules([from.clone(), to.clone()]);
        if exists {
            out.push(Finding::supported(
                format!("diagram draws `{from}` -> `{to}`"),
                d.origin.clone(),
                prov,
            ));
        } else {
            out.push(
                Finding::problem(
                    Verdict::Contradicted,
                    format!("diagram draws `{from}` -> `{to}`"),
                    d.origin.clone(),
                    format!(
                        "Phantom dependency: the {} diagram draws an edge `{from}` -> `{to}`, but no import connects those modules.",
                        "mermaid"
                    ),
                )
                .anchored(prov)
                .with_refs(vec![format!("{from} -> {to}")]),
            );
        }
    }

    // 2. Stale boxes: a box that clearly names a code module that is gone. Fuzzy
    //    resolution only *reduces* this set (more boxes ground), so it stays safe.
    for (text, resolution) in &node_res {
        if resolution.grounds() {
            continue; // names real code (exact, fuzzy, or ambiguous) → not stale
        }
        let g = ground_label(text);
        if module_intent(g) {
            out.push(Finding::problem(
                Verdict::Stale,
                format!("diagram box `{text}`"),
                d.origin.clone(),
                format!(
                    "Stale box: the {} diagram contains a box `{text}` that resolves to no module in the repo.",
                    "mermaid"
                ),
            ));
        }
    }

    // 3. Missing arrows: a real import between two boxes that are *both* already
    //    drawn, yet no edge connects them. Bounded to depicted modules, so it
    //    never fires for components the author chose to omit.
    //    Exact-unique only: fuzzy/ambiguous boxes must not invent "you forgot an
    //    edge", since abstract diagrams omit edges intentionally (highest-FP class).
    let mut seen = HashSet::new();
    for me in &index.module_edges {
        let from_drawn = node_res.iter().any(|(_, r)| {
            r.exact_module()
                .is_some_and(|g| matches(&me.from_module, g))
        });
        let to_drawn = node_res
            .iter()
            .any(|(_, r)| r.exact_module().is_some_and(|g| matches(&me.to_module, g)));
        if !from_drawn || !to_drawn {
            continue;
        }
        let drawn = d.edges.iter().any(|e| {
            let ft = ground_label(&d.text(&e.from)).to_string();
            let tt = ground_label(&d.text(&e.to)).to_string();
            (matches(&me.from_module, &ft) && matches(&me.to_module, &tt))
                || (!e.directed && matches(&me.from_module, &tt) && matches(&me.to_module, &ft))
        });
        if !drawn && seen.insert((me.from_module.clone(), me.to_module.clone())) {
            out.push(
                Finding::problem(
                    Verdict::Undocumented,
                    format!("import `{}` -> `{}`", me.from_module, me.to_module),
                    d.origin.clone(),
                    format!(
                        "Missing arrow: `{}` imports `{}` and both are drawn in the {} diagram, but no edge connects them.",
                        me.from_module,
                        me.to_module,
                        "mermaid"
                    ),
                )
                .anchored(Provenance::modules([
                    me.from_module.clone(),
                    me.to_module.clone(),
                ]))
                .with_refs(vec![format!("{} -> {}", me.from_module, me.to_module)]),
            );
        }
    }

    out
}

/// True if a box label unambiguously denotes a code module *path* (so an
/// ungrounded one is a stale reference, not a conceptual box). Deliberately
/// conservative: only a clean path/namespace token with a separator counts.
/// A bare word (`User`, `DB`), a URL route (`/items/public/`), a decision-node
/// label (`needed=False<br/>ok`), or call syntax is left alone to keep Layer 1
/// zero-FP. Pass the [`ground_label`]-normalized text so a trailing `.ts`/`.py`
/// doesn't read as a path dot.
fn module_intent(text: &str) -> bool {
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return false;
    }
    // URL routes / fragments are not code module paths.
    if text.starts_with('/') || text.contains("://") {
        return false;
    }
    // Markup, decision-node labels, call/query syntax; none belong in a path.
    if text.contains([
        '=', '<', '>', '{', '}', '(', ')', '\\', '?', '#', '&', '"', '\'',
    ]) {
        return false;
    }
    // What remains must carry a path or namespace separator.
    text.contains('/') || text.contains("::")
}

#[cfg(test)]
mod tests;
