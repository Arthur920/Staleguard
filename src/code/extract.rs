//! Per-file extraction: symbols via tree-sitter-tags, dependency edges via a
//! small per-language import query.

use std::ops::Range;
use std::path::Path;

use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};
use tree_sitter_tags::TagsContext;

use crate::code::lang::{self, Language};
use crate::code::symbol::{DepEdge, Facts, Span, Symbol, SymbolKind, Visibility};

/// A reference whose enclosing definition has been resolved intra-file. `from`
/// is the enclosing symbol's `qualified_name` (or the module path for top-level
/// references); `name` is the referenced identifier, resolved to a target symbol
/// globally in [`CodeIndex::build`]. Internal to the extractor.
pub(crate) struct RawRef {
    pub from: String,
    pub name: String,
}

/// Extract symbols, dependency edges, and raw references from one file.
/// Unparseable files and unsupported languages yield empty results rather than
/// erroring.
pub fn extract_file(path: &Path, repo_root: &Path) -> (Vec<Symbol>, Vec<DepEdge>, Vec<RawRef>) {
    let Some(language) = Language::from_path(path) else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let Ok(source) = std::fs::read(path) else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let module = lang::module_path(path, repo_root);
    let rel = path
        .strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    let (symbols, refs) = extract_symbols_and_refs(language, &source, &module, &rel);
    let edges = extract_edges(language, &source, &module);
    (symbols, edges, refs)
}

fn extract_symbols_and_refs(
    language: Language,
    source: &[u8],
    module: &str,
    rel: &str,
) -> (Vec<Symbol>, Vec<RawRef>) {
    let Ok(config) = language.tags_config() else {
        return (Vec::new(), Vec::new());
    };
    let mut ctx = TagsContext::new();
    let Ok((tags, _)) = ctx.generate_tags(&config, source, None) else {
        return (Vec::new(), Vec::new());
    };

    let text = String::from_utf8_lossy(source);
    let lines: Vec<&str> = text.lines().collect();

    let mut symbols = Vec::new();
    // (full byte range of a definition, its qualified_name) for the innermost-
    // enclosing lookup below. Definition ranges cover the body (the tag node is
    // the whole `function_item`/`class` etc.), unlike `Tag.span` (name only).
    let mut defs: Vec<(Range<usize>, String)> = Vec::new();
    // (referenced name, byte position) — enclosing symbol resolved after the loop.
    let mut ref_sites: Vec<(String, usize)> = Vec::new();

    for tag in tags {
        let Ok(tag) = tag else { continue };
        let name = String::from_utf8_lossy(&source[tag.name_range.clone()]).into_owned();
        if !tag.is_definition {
            ref_sites.push((name, tag.name_range.start));
            continue;
        }
        let qualified_name = format!("{module}::{name}");
        let kind = map_kind(config.syntax_type_name(tag.syntax_type_id));
        let start_row = tag.span.start.row;
        let decl_line = lines.get(start_row).map(|l| l.trim().to_string());
        let visibility = classify_visibility(language, decl_line.as_deref().unwrap_or(""), &name);

        defs.push((tag.range.clone(), qualified_name.clone()));
        symbols.push(Symbol {
            qualified_name,
            name,
            kind,
            visibility,
            module: module.to_string(),
            span: Span {
                path: rel.to_string(),
                start_line: start_row + 1,
                end_line: tag.span.end.row + 1,
            },
            signature: decl_line,
            doc: tag.docs.clone(),
            // Behavioral-fact population is deferred to the drift-fingerprint
            // consumer, which needs a per-symbol AST walk. Plumbing is in place.
            facts: Facts::default(),
        });
    }

    let refs = ref_sites
        .into_iter()
        .map(|(name, pos)| RawRef {
            from: enclosing(&defs, pos).unwrap_or(module).to_string(),
            name,
        })
        .collect();

    (symbols, refs)
}

/// The innermost definition whose byte range contains `pos`. Among containing
/// ranges the one with the largest `start` is the most deeply nested.
fn enclosing(defs: &[(Range<usize>, String)], pos: usize) -> Option<&str> {
    defs.iter()
        .filter(|(r, _)| r.start <= pos && pos < r.end)
        .max_by_key(|(r, _)| r.start)
        .map(|(_, q)| q.as_str())
}

fn extract_edges(language: Language, source: &[u8], module: &str) -> Vec<DepEdge> {
    let ts_lang = language.ts_language();
    let Ok(query) = Query::new(&ts_lang, language.import_query()) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source);
    let mut edges = Vec::new();
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let raw = cap.node.utf8_text(source).unwrap_or("");
            let target = normalize_import(raw);
            if !target.is_empty() {
                edges.push(DepEdge {
                    from_module: module.to_string(),
                    to_module: target,
                });
            }
        }
    }
    edges
}

fn map_kind(name: &str) -> SymbolKind {
    match name {
        "function" => SymbolKind::Function,
        "method" | "constructor" => SymbolKind::Method,
        "struct" => SymbolKind::Struct,
        "class" => SymbolKind::Class,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "interface" => SymbolKind::Interface,
        "module" => SymbolKind::Module,
        "constant" => SymbolKind::Constant,
        "field" | "property" | "member" => SymbolKind::Field,
        other => SymbolKind::Other(other.to_string()),
    }
}

fn classify_visibility(language: Language, decl_line: &str, name: &str) -> Visibility {
    let has_word = |w: &str| decl_line.split(|c: char| !c.is_alphanumeric()).any(|t| t == w);
    match language {
        Language::Rust => {
            if decl_line.split_whitespace().any(|w| w == "pub" || w.starts_with("pub(")) {
                Visibility::Public
            } else {
                Visibility::Private
            }
        }
        Language::Python => {
            if name.starts_with('_') {
                Visibility::Private
            } else {
                Visibility::Public
            }
        }
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            if has_word("export") {
                Visibility::Public
            } else {
                Visibility::Internal
            }
        }
        Language::Java => {
            if has_word("public") {
                Visibility::Public
            } else if has_word("private") {
                Visibility::Private
            } else {
                Visibility::Internal
            }
        }
    }
}

fn normalize_import(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vis_of<'a>(syms: &'a [Symbol], name: &str) -> Option<Visibility> {
        syms.iter().find(|s| s.name == name).map(|s| s.visibility)
    }

    fn has_edge(edges: &[DepEdge], target_contains: &str) -> bool {
        edges.iter().any(|e| e.to_module.contains(target_contains))
    }

    fn has_ref(refs: &[RawRef], from: &str, name: &str) -> bool {
        refs.iter().any(|r| r.from == from && r.name == name)
    }

    #[test]
    fn rust_symbols_and_edges() {
        let src = b"pub fn foo() {}\nfn bar() {}\nuse std::fmt;\n";
        let (syms, _refs) = extract_symbols_and_refs(Language::Rust, src, "m", "m.rs");
        assert_eq!(vis_of(&syms, "foo"), Some(Visibility::Public));
        assert_eq!(vis_of(&syms, "bar"), Some(Visibility::Private));
        let edges = extract_edges(Language::Rust, src, "m");
        assert!(has_edge(&edges, "std::fmt"));
    }

    #[test]
    fn python_symbols_and_edges() {
        let src = b"def foo():\n    pass\ndef _bar():\n    pass\nimport os\n";
        let (syms, _refs) = extract_symbols_and_refs(Language::Python, src, "m", "m.py");
        assert_eq!(vis_of(&syms, "foo"), Some(Visibility::Public));
        assert_eq!(vis_of(&syms, "_bar"), Some(Visibility::Private));
        let edges = extract_edges(Language::Python, src, "m");
        assert!(has_edge(&edges, "os"));
    }

    #[test]
    fn javascript_symbols_and_edges() {
        let src = b"export function foo() {}\nfunction bar() {}\nimport x from \"./mod\";\n";
        let (syms, _refs) = extract_symbols_and_refs(Language::JavaScript, src, "m", "m.js");
        assert_eq!(vis_of(&syms, "foo"), Some(Visibility::Public));
        assert_eq!(vis_of(&syms, "bar"), Some(Visibility::Internal));
        let edges = extract_edges(Language::JavaScript, src, "m");
        assert!(has_edge(&edges, "./mod"));
    }

    #[test]
    fn typescript_symbols_and_edges() {
        let src = b"export class A {}\nimport { x } from \"./mod\";\n";
        let (syms, _refs) = extract_symbols_and_refs(Language::TypeScript, src, "m", "m.ts");
        assert_eq!(vis_of(&syms, "A"), Some(Visibility::Public));
        let edges = extract_edges(Language::TypeScript, src, "m");
        assert!(has_edge(&edges, "./mod"));
    }

    #[test]
    fn java_symbols_and_edges() {
        let src = b"import a.b.C;\npublic class A {\n  public void m() {}\n}\n";
        let (syms, _refs) = extract_symbols_and_refs(Language::Java, src, "m", "m.java");
        assert_eq!(vis_of(&syms, "A"), Some(Visibility::Public));
        let edges = extract_edges(Language::Java, src, "m");
        assert!(has_edge(&edges, "a.b.C"));
    }

    #[test]
    fn call_resolves_to_enclosing_caller() {
        // `foo`'s body calls `bar` -> a reference from `m::foo` named `bar`.
        let src = b"fn bar() {}\nfn foo() {\n    bar();\n}\n";
        let (_syms, refs) = extract_symbols_and_refs(Language::Rust, src, "m", "m.rs");
        assert!(has_ref(&refs, "m::foo", "bar"));
    }

    #[test]
    fn recursive_call_is_kept_as_self_ref_site() {
        // The self-call is captured with from == name; the self-edge is dropped
        // later, globally, in `resolve_refs`.
        let src = b"fn foo() {\n    foo();\n}\n";
        let (_syms, refs) = extract_symbols_and_refs(Language::Rust, src, "m", "m.rs");
        assert!(has_ref(&refs, "m::foo", "foo"));
    }

    #[test]
    fn top_level_reference_falls_back_to_module() {
        // A call outside any definition has the module path as its `from`.
        let src = b"fn foo() {}\nconst N: usize = foo();\n";
        let (_syms, refs) = extract_symbols_and_refs(Language::Rust, src, "m", "m.rs");
        assert!(refs.iter().any(|r| r.name == "foo" && r.from == "m"));
    }
}
