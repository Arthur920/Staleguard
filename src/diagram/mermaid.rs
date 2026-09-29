//! Mermaid `graph`/`flowchart` parser. Other mermaid kinds (sequence, class,
//! ER, state) are recognized and skipped; they need symbol/call-graph
//! alignment, not a module edge-diff.

use std::sync::OnceLock;

use regex::Regex;

use super::{Diagram, Edge, Node};

pub(super) fn parse(body: &str, origin: &str) -> Option<Diagram> {
    // Find the diagram header (first non-empty, non-directive line).
    let header = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?;
    let first = header.split_whitespace().next().unwrap_or("");
    if !matches!(first, "graph" | "flowchart") {
        return None; // sequenceDiagram / classDiagram / erDiagram / … → skip
    }

    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();

    let mut register = |id: &str, label: Option<&str>| {
        let id = id.to_string();
        match nodes.iter_mut().find(|n| n.id == id) {
            Some(existing) => {
                if let Some(label) = label {
                    existing.label = label.to_string();
                }
            }
            None => {
                let label = label.unwrap_or(&id).to_string();
                nodes.push(Node { id, label });
            }
        }
    };

    // Walk every statement after the header. Statements may be `;`-separated.
    let mut lines = body.lines();
    // Skip up to and including the header line.
    for line in lines.by_ref() {
        if line.trim() == header {
            break;
        }
    }
    let remaining: String = std::iter::once(header_tail(header))
        .chain(lines.map(str::to_string))
        .collect::<Vec<_>>()
        .join("\n");

    for stmt in remaining.split(['\n', ';']) {
        let stmt = stmt.trim();
        if stmt.is_empty()
            || stmt.starts_with("%%")
            || stmt.starts_with("subgraph")
            || stmt == "end"
        {
            continue;
        }
        // Register declared nodes (`A[Label]`, `A(Label)`, `A{Label}`, `A>Label]`).
        for c in node_decl_re().captures_iter(stmt) {
            register(&c[1], Some(c[2].trim_matches('"').trim()));
        }
        // Strip pipe labels (`-->|text|`) and bracket labels so only ids and
        // arrows remain, then split the chain on arrow operators. Stripping is
        // length-preserving (spaces), so non-real label spans recorded on the
        // original statement still index into the stripped one.
        let non_real: Vec<(usize, usize)> = pipe_re()
            .find_iter(stmt)
            .filter(|m| super::non_real_label(m.as_str().trim_matches('|')))
            .map(|m| (m.start(), m.end()))
            .collect();
        let stripped = strip_labels(stmt);
        parse_chain(&stripped, &non_real, &mut register, &mut edges);
    }
    let _ = register; // drop closure, releasing the &mut borrow of `nodes`

    if edges.is_empty() && nodes.is_empty() {
        return None;
    }
    Some(Diagram {
        nodes,
        edges,
        origin: origin.to_string(),
    })
}

/// Everything on the header line after the `graph TD` / `flowchart LR` prefix;
/// mermaid allows the first edge to share the header line.
fn header_tail(header: &str) -> String {
    let header = header.trim();
    let rest = header
        .strip_prefix("graph")
        .or_else(|| header.strip_prefix("flowchart"))
        .unwrap_or("")
        .trim_start();
    // Drop an optional standalone direction keyword (TD/TB/BT/LR/RL).
    for dir in ["TD", "TB", "BT", "LR", "RL"] {
        if let Some(after) = rest.strip_prefix(dir) {
            if after.is_empty() || after.starts_with([' ', '\t', ';']) {
                return after.trim_start_matches([' ', '\t', ';']).to_string();
            }
        }
    }
    rest.to_string()
}

/// Split a label-stripped statement into edges along its arrow operators,
/// handling chains like `A --> B --> C`.
fn parse_chain<F: FnMut(&str, Option<&str>)>(
    stripped: &str,
    non_real: &[(usize, usize)],
    register: &mut F,
    edges: &mut Vec<Edge>,
) {
    let arrows: Vec<_> = arrow_re().find_iter(stripped).collect();
    if arrows.is_empty() {
        return;
    }
    let mut left = leading_id(&stripped[..arrows[0].start()]);
    for (k, arr) in arrows.iter().enumerate() {
        let next_start = arrows
            .get(k + 1)
            .map(|a| a.start())
            .unwrap_or(stripped.len());
        let segment = &stripped[arr.end()..next_start];
        let right = leading_id(segment);
        if let (Some(l), Some(r)) = (left.as_deref(), right.as_deref()) {
            register(l, None);
            register(r, None);
            // A pipe label sits between its arrow and the next node/arrow.
            let asserted = !non_real
                .iter()
                .any(|&(s, e)| s >= arr.end() && e <= next_start);
            edges.push(Edge {
                from: l.to_string(),
                to: r.to_string(),
                directed: arr.as_str().contains('>'),
                asserted,
            });
        }
        left = right;
    }
}

/// Replace bracket labels (`[..]`, `(..)`, `{..}`, `>..]`) and pipe labels
/// (`|..|`) with same-length runs of spaces, so the residue is just
/// `id arrow id` and byte offsets stay aligned with the original statement.
fn strip_labels(stmt: &str) -> String {
    let blank = |c: &regex::Captures| " ".repeat(c[0].len());
    let no_pipe = pipe_re().replace_all(stmt, blank);
    bracket_re().replace_all(&no_pipe, blank).into_owned()
}

/// Leading identifier of a node reference (`A`, `api`, `web-ui`).
fn leading_id(s: &str) -> Option<String> {
    let s = s.trim_start();
    let id: String = s
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

fn node_decl_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // id followed by a bracketed label: `A[..]`, `A(..)`, `A{..}`.
        Regex::new(r"([A-Za-z_][\w-]*)\s*(?:\[+|\(+|\{+)\s*([^\]\)\}]+?)\s*(?:\]+|\)+|\}+)")
            .unwrap()
    })
}

fn arrow_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // `-->`, `---`, `-.->`, `-.-`, `==>`, `===`, with or without a trailing `>`.
    RE.get_or_init(|| Regex::new(r"[-=.]{2,}>?|[-=.]*>").unwrap())
}

fn pipe_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\|[^|]*\|").unwrap())
}

fn bracket_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:\[+|\(+|\{+)[^\]\)\}]*(?:\]+|\)+|\}+)").unwrap())
}
