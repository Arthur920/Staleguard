//! Per-file extraction: symbols via tree-sitter-tags, plus behavioral facts from
//! each definition's AST node.

use std::path::Path;

use tree_sitter::{Node, Parser, Tree};
use tree_sitter_tags::TagsContext;

use crate::code::facts;
use crate::code::lang::{self, Language};
use crate::code::symbol::{Span, Symbol, SymbolKind, Visibility};

/// Extract symbols from one file. Unparseable files and unsupported languages
/// yield no symbols rather than erroring.
pub fn extract_file(path: &Path, repo_root: &Path) -> Vec<Symbol> {
    let Some(language) = Language::from_path(path) else {
        return Vec::new();
    };
    let Ok(source) = std::fs::read(path) else {
        return Vec::new();
    };
    let module = lang::module_path(path, repo_root);
    let rel = path
        .strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    // tree-sitter-tags parses internally without exposing the tree, so parse
    // once more for fact extraction.
    let tree = parse_tree(language, &source);
    symbols(
        language,
        &source,
        &module,
        &rel,
        tree.as_ref().map(|t| t.root_node()),
    )
}

/// Parse `source` into a syntax tree, or `None` if the language/parse fails.
fn parse_tree(language: Language, source: &[u8]) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&language.ts_language()).ok()?;
    parser.parse(source, None)
}

fn symbols(
    language: Language,
    source: &[u8],
    module: &str,
    rel: &str,
    root: Option<Node>,
) -> Vec<Symbol> {
    let Some(config) = language.tags_config_cached() else {
        return Vec::new();
    };
    let mut ctx = TagsContext::new();
    let Ok((tags, _)) = ctx.generate_tags(config, source, None) else {
        return Vec::new();
    };

    let text = String::from_utf8_lossy(source);
    let lines: Vec<&str> = text.lines().collect();

    // Tags give byte ranges (`tag.range`) but not AST nodes; each definition's
    // node is resolved by byte range below for behavioral facts and body span.
    let mut symbols = Vec::new();
    for tag in tags {
        let Ok(tag) = tag else { continue };
        if !tag.is_definition {
            continue;
        }
        let name = String::from_utf8_lossy(&source[tag.name_range.clone()]).into_owned();
        let qualified_name = format!("{module}::{name}");
        let mut kind = map_kind(config.syntax_type_name(tag.syntax_type_id));
        let start_row = tag.span.start.row;
        let decl_line = lines.get(start_row).map(|l| l.trim().to_string());
        let visibility = classify_visibility(language, decl_line.as_deref().unwrap_or(""), &name);

        let span = Span {
            path: rel.to_string(),
            start_line: start_row + 1,
            end_line: tag.span.end.row + 1,
        };
        // Resolve the definition node (covers the body) for facts + body_span.
        let def_node = root.and_then(|r| {
            r.descendant_for_byte_range(tag.range.start, tag.range.end.saturating_sub(1))
        });
        let (body_span, fact_data) = match def_node {
            Some(node) => (
                Span {
                    path: rel.to_string(),
                    start_line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                },
                facts::extract(node, source, language, decl_line.clone()),
            ),
            None => (
                span.clone(),
                facts::extract_signature_only(decl_line.clone()),
            ),
        };

        // tree-sitter-tags collapses enums into the `class` tag kind, so detect
        // them from the AST node and correct the kind.
        if def_node.is_some_and(|n| {
            is_enum_node(n.kind()) || (language == Language::Python && is_python_enum(n, source))
        }) {
            kind = SymbolKind::Enum;
        }

        symbols.push(Symbol {
            qualified_name,
            name,
            kind,
            visibility,
            module: module.to_string(),
            span,
            body_span,
            signature: decl_line,
            doc: tag.docs.clone(),
            facts: fact_data,
        });
    }
    symbols
}

/// Whether an AST node kind denotes a first-class enum definition (Rust
/// `enum_item`, Java/TS `enum_declaration`). Python enums are class-based and
/// handled separately by [`is_python_enum`].
fn is_enum_node(kind: &str) -> bool {
    matches!(kind, "enum_item" | "enum_declaration")
}

/// Whether a Python `class_definition` derives from an enum base
/// (`Enum`/`IntEnum`/`StrEnum`/`Flag`/`IntFlag`, bare or `enum.`-qualified).
fn is_python_enum(node: tree_sitter::Node, source: &[u8]) -> bool {
    if node.kind() != "class_definition" {
        return false;
    }
    let Some(supers) = node.child_by_field_name("superclasses") else {
        return false;
    };
    let mut cursor = supers.walk();
    for c in supers.children(&mut cursor) {
        let base = c.utf8_text(source).unwrap_or("");
        let leaf = base.rsplit('.').next().unwrap_or(base);
        if matches!(leaf, "Enum" | "IntEnum" | "StrEnum" | "Flag" | "IntFlag") {
            return true;
        }
    }
    false
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
    let has_word = |w: &str| {
        decl_line
            .split(|c: char| !c.is_alphanumeric())
            .any(|t| t == w)
    };
    match language {
        Language::Rust => {
            if decl_line
                .split_whitespace()
                .any(|w| w == "pub" || w.starts_with("pub("))
            {
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

/// Test-only wrapper that parses internally, so unit tests can drive
/// extraction from a raw source string.
#[cfg(test)]
fn extract_symbols(language: Language, source: &[u8], module: &str, rel: &str) -> Vec<Symbol> {
    let tree = parse_tree(language, source);
    symbols(
        language,
        source,
        module,
        rel,
        tree.as_ref().map(|t| t.root_node()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vis_of(syms: &[Symbol], name: &str) -> Option<Visibility> {
        syms.iter().find(|s| s.name == name).map(|s| s.visibility)
    }

    #[test]
    fn visibility_per_language() {
        let cases: [(Language, &[u8], &str, Visibility); 7] = [
            (
                Language::Rust,
                b"pub fn foo() {}",
                "foo",
                Visibility::Public,
            ),
            (Language::Rust, b"fn bar() {}", "bar", Visibility::Private),
            (
                Language::Python,
                b"def foo():\n    pass\n",
                "foo",
                Visibility::Public,
            ),
            (
                Language::Python,
                b"def _bar():\n    pass\n",
                "_bar",
                Visibility::Private,
            ),
            (
                Language::JavaScript,
                b"export function foo() {}",
                "foo",
                Visibility::Public,
            ),
            (
                Language::JavaScript,
                b"function bar() {}",
                "bar",
                Visibility::Internal,
            ),
            (
                Language::TypeScript,
                b"export class A {}",
                "A",
                Visibility::Public,
            ),
        ];
        for (lang, src, name, want) in cases {
            let syms = extract_symbols(lang, src, "m", "m");
            assert_eq!(vis_of(&syms, name), Some(want), "{lang:?} {name}");
        }
        let java = extract_symbols(
            Language::Java,
            b"public class A {\n  public void m() {}\n}\n",
            "m",
            "m.java",
        );
        assert_eq!(vis_of(&java, "A"), Some(Visibility::Public));
    }

    #[test]
    fn rust_enum_is_an_enum() {
        let src = b"pub enum State {\n    Idle,\n    Running,\n    Done,\n}\n";
        let syms = extract_symbols(Language::Rust, src, "m", "m.rs");
        let en = syms.iter().find(|s| s.name == "State").unwrap();
        assert_eq!(en.kind, SymbolKind::Enum);
    }

    #[test]
    fn python_class_enum_is_an_enum() {
        let src = b"from enum import Enum\n\
                    class State(Enum):\n    IDLE = 1\n    RUNNING = 2\n    DONE = 3\n";
        let syms = extract_symbols(Language::Python, src, "m", "m.py");
        let en = syms.iter().find(|s| s.name == "State").unwrap();
        assert_eq!(en.kind, SymbolKind::Enum);
    }

    #[test]
    fn python_plain_class_is_not_an_enum() {
        let src = b"class Plain:\n    X = 1\n    def m(self):\n        pass\n";
        let syms = extract_symbols(Language::Python, src, "m", "m.py");
        let cls = syms.iter().find(|s| s.name == "Plain").unwrap();
        assert_ne!(cls.kind, SymbolKind::Enum);
    }
}
