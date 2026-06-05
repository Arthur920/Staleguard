//! Layer 1: architecture-rule fitness functions.
//!
//! Docs constantly state architectural invariants — "`controllers` must not
//! import `db`", "`domain` depends on nothing", "no direct use of `eval`".
//! These are negative/absence claims the other checks can't see. Here we
//! extract such rules from doc prose, compile each to a dependency-graph or
//! source query, and verify it against the resolved module graph. A violation
//! is a hard `contradicted` verdict — no ML.
//!
//! Zero false positives: a rule whose module operands don't resolve to any real
//! module is skipped rather than guessed, and module matching is grounded
//! against the index's `module_set`.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::code::lang;
use crate::code::CodeIndex;
use crate::findings::{Finding, Verdict};

/// A compiled architectural invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// `from` must not depend on `to`.
    ForbidEdge { from: String, to: String },
    /// `module` may depend only on `allowed` (empty ⇒ "depends on nothing").
    Layer { module: String, allowed: Vec<String> },
    /// `symbol` must not appear outside the `except` modules.
    ForbidSymbol { symbol: String, except: Vec<String> },
}

/// A rule plus where it came from (a doc `path:line`, or the rules file).
#[derive(Debug, Clone)]
pub struct SourcedRule {
    pub rule: Rule,
    pub origin: String,
}

// ---- rule sources ---------------------------------------------------------

/// Parse architectural rules out of one markdown doc's prose. Operands are
/// always backtick-quoted; phrasings are deliberately narrow to avoid matching
/// ordinary prose.
pub fn extract_prose_rules(markdown: &str, doc_path: &str) -> Vec<SourcedRule> {
    let mut rules = Vec::new();
    for (i, line) in markdown.lines().enumerate() {
        // Drop double-quoted spans: an author quoting an *example* rule
        // ("no `eval`") is describing the feature, not stating an enforced rule.
        let line = quoted_re().replace_all(line, "");
        let line = line.as_ref();
        let origin = format!("{doc_path}:{}", i + 1);
        let mut push = |rule| rules.push(SourcedRule { rule, origin: origin.clone() });

        for c in forbid_edge_re().captures_iter(line) {
            push(Rule::ForbidEdge { from: c[1].to_string(), to: c[2].to_string() });
        }
        for c in never_edge_re().captures_iter(line) {
            push(Rule::ForbidEdge { from: c[1].to_string(), to: c[2].to_string() });
        }
        for c in depends_nothing_re().captures_iter(line) {
            push(Rule::Layer { module: c[1].to_string(), allowed: Vec::new() });
        }
        for c in only_depends_re().captures_iter(line) {
            let allowed = backtick_tokens(&c[2]);
            if !allowed.is_empty() {
                push(Rule::Layer { module: c[1].to_string(), allowed });
            }
        }
        for c in forbid_symbol_re().captures_iter(line) {
            push(Rule::ForbidSymbol {
                symbol: c[1].to_string(),
                except: except_modules(line),
            });
        }
    }
    rules
}

// ---- checking -------------------------------------------------------------

/// Verify every rule against the index, returning `contradicted` findings for
/// violations.
pub fn check(rules: &[SourcedRule], index: &CodeIndex, repo_root: &Path) -> Vec<Finding> {
    let modules = index.module_set();
    let mut findings = Vec::new();
    for sr in rules {
        match &sr.rule {
            Rule::ForbidEdge { from, to } => {
                check_forbid_edge(sr, from, to, index, &modules, &mut findings)
            }
            Rule::Layer { module, allowed } => {
                check_layer(sr, module, allowed, index, &modules, &mut findings)
            }
            Rule::ForbidSymbol { symbol, except } => {
                check_forbid_symbol(sr, symbol, except, repo_root, &mut findings)
            }
        }
    }
    findings
}

fn check_forbid_edge(
    sr: &SourcedRule,
    from: &str,
    to: &str,
    index: &CodeIndex,
    modules: &HashSet<String>,
    out: &mut Vec<Finding>,
) {
    if !grounded(from, modules) || !grounded(to, modules) {
        return; // operand names no real module — unverifiable, don't guess.
    }
    for e in &index.module_edges {
        if matches(&e.from_module, from) && matches(&e.to_module, to) {
            out.push(violation(
                sr,
                format!("`{from}` must not import `{to}`"),
                format!("`{}` imports `{}`.", e.from_module, e.to_module),
                &e.from_module,
                &e.to_module,
            ));
        }
    }
}

fn check_layer(
    sr: &SourcedRule,
    module: &str,
    allowed: &[String],
    index: &CodeIndex,
    modules: &HashSet<String>,
    out: &mut Vec<Finding>,
) {
    if !grounded(module, modules) {
        return;
    }
    for e in &index.module_edges {
        if !matches(&e.from_module, module) {
            continue;
        }
        // Edges within the module's own subtree are not external dependencies.
        if matches(&e.to_module, module) {
            continue;
        }
        if allowed.iter().any(|a| matches(&e.to_module, a)) {
            continue;
        }
        let claim = if allowed.is_empty() {
            format!("`{module}` depends on nothing")
        } else {
            format!("`{module}` may depend only on {}", quote_list(allowed))
        };
        out.push(violation(
            sr,
            claim,
            format!("`{}` imports `{}`.", e.from_module, e.to_module),
            &e.from_module,
            &e.to_module,
        ));
    }
}

fn check_forbid_symbol(
    sr: &SourcedRule,
    symbol: &str,
    except: &[String],
    repo_root: &Path,
    out: &mut Vec<Finding>,
) {
    let matcher = symbol_matcher(symbol);
    for file in lang::code_files(repo_root) {
        let module = lang::module_path(&file, repo_root);
        if except.iter().any(|e| matches(&module, e)) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            if matcher.is_match(line) {
                let at = format!("{module}:{}", i + 1);
                out.push(Finding {
                    verdict: Verdict::Contradicted,
                    claim: format!("forbids `{symbol}`"),
                    doc_path: sr.origin.clone(),
                    detail: format!("Rule forbids `{symbol}`, but it appears in `{at}`."),
                    layer: 1,
                    code_refs: vec![at],
                });
            }
        }
    }
}

/// A finding for a violated module-graph rule.
fn violation(sr: &SourcedRule, claim: String, detail: String, from: &str, to: &str) -> Finding {
    Finding {
        verdict: Verdict::Contradicted,
        claim,
        doc_path: sr.origin.clone(),
        detail: format!("Rule violated: {detail}"),
        layer: 1,
        code_refs: vec![format!("{from} -> {to}")],
    }
}

// ---- matching helpers -----------------------------------------------------

/// A module path matches an operand by exact equality, subtree prefix
/// (`op/…`), leaf suffix (`…/op`), or interior segment (`…/op/…`) — so a
/// conceptual name (`controllers`) matches a real path (`src/controllers`).
fn matches(module: &str, operand: &str) -> bool {
    let op = operand.trim_matches('/');
    module == op
        || module.starts_with(&format!("{op}/"))
        || module.ends_with(&format!("/{op}"))
        || module.contains(&format!("/{op}/"))
}

/// True if an operand matches at least one real module.
fn grounded(operand: &str, modules: &HashSet<String>) -> bool {
    modules.iter().any(|m| matches(m, operand))
}

/// Identifier symbols match on word boundaries; anything else (e.g.
/// `os.environ`) matches as a literal substring.
fn symbol_matcher(symbol: &str) -> Regex {
    if symbol.chars().all(|c| c.is_alphanumeric() || c == '_') {
        Regex::new(&format!(r"\b{}\b", regex::escape(symbol))).unwrap()
    } else {
        Regex::new(&regex::escape(symbol)).unwrap()
    }
}

fn quote_list(items: &[String]) -> String {
    items
        .iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// All backtick-quoted tokens in a string.
fn backtick_tokens(s: &str) -> Vec<String> {
    backtick_re()
        .captures_iter(s)
        .map(|c| c[1].to_string())
        .collect()
}

/// Backtick tokens following an `outside`/`except` keyword on a forbid-symbol
/// line, forming the rule's exception list.
fn except_modules(line: &str) -> Vec<String> {
    let lower = line.to_lowercase();
    let Some(pos) = ["outside", "except"].iter().find_map(|kw| lower.find(kw)) else {
        return Vec::new();
    };
    backtick_tokens(&line[pos..])
}

// ---- prose patterns -------------------------------------------------------

fn forbid_edge_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"`([^`]+)`(?:\s+(?:layer|module|package|crate|component))?\s+(?:must|should|may|can|cannot|does|do)\s+not\s+(?:import|imports|depend\s+on|depends\s+on|use|uses|reference|references|access|accesses|touch|touches)\s+(?:the\s+)?`([^`]+)`",
        )
        .unwrap()
    })
}

fn never_edge_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"`([^`]+)`(?:\s+(?:layer|module|package|crate|component))?\s+never\s+(?:imports?|depends?\s+on|uses?|references?)\s+(?:the\s+)?`([^`]+)`",
        )
        .unwrap()
    })
}

fn depends_nothing_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"`([^`]+)`(?:\s+(?:layer|module|package|crate|component))?\s+(?:depends?\s+on|imports?|has)\s+(?:nothing|no\s+(?:dependencies|deps|imports))",
        )
        .unwrap()
    })
}

fn only_depends_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"`([^`]+)`(?:\s+(?:layer|module|package|crate|component))?\s+(?:must\s+)?only\s+(?:depends?\s+on|imports?)\s+(.*)").unwrap()
    })
}

/// Forbid-symbol phrasings, all requiring a use/call signal so a bare "no
/// `config`" in prose is not mistaken for a rule.
fn forbid_symbol_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?:(?:must|should|may)\s+not\s+(?:use|call|invoke|reference)|don'?t\s+(?:use|call)|never\s+(?:use|call)|no\s+(?:direct|raw)|no\s+(?:use|usage|calls?)\s+(?:of|to))\s+`([^`]+)`",
        )
        .unwrap()
    })
}

fn backtick_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([^`]+)`").unwrap())
}

/// A double-quoted span (straight or curly quotes).
fn quoted_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#""[^"]*"|“[^”]*”"#).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::symbol::DepEdge;

    fn edge(from: &str, to: &str) -> DepEdge {
        DepEdge {
            from_module: from.to_string(),
            to_module: to.to_string(),
        }
    }

    fn index(edges: Vec<DepEdge>) -> CodeIndex {
        // module_set comes from from_module + symbol modules; mirror endpoints
        // into edges so both ends ground.
        CodeIndex {
            symbols: vec![],
            edges: edges
                .iter()
                .flat_map(|e| {
                    [
                        edge(&e.from_module, "x"),
                        edge(&e.to_module, "x"),
                    ]
                })
                .collect(),
            module_edges: edges,
            ref_edges: vec![],
        }
    }

    fn rule(r: Rule) -> Vec<SourcedRule> {
        vec![SourcedRule { rule: r, origin: "rules".into() }]
    }

    #[test]
    fn forbid_edge_violation_is_contradicted() {
        let idx = index(vec![edge("src/api", "src/db")]);
        let rules = rule(Rule::ForbidEdge { from: "src/api".into(), to: "src/db".into() });
        let f = check(&rules, &idx, Path::new("."));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].verdict, Verdict::Contradicted);
        assert_eq!(f[0].code_refs, vec!["src/api -> src/db"]);
    }

    #[test]
    fn forbid_edge_clean_repo_passes() {
        let idx = index(vec![edge("src/api", "src/domain")]);
        let rules = rule(Rule::ForbidEdge { from: "src/api".into(), to: "src/db".into() });
        assert!(check(&rules, &idx, Path::new(".")).is_empty());
    }

    #[test]
    fn ungrounded_operand_is_skipped() {
        let idx = index(vec![edge("src/api", "src/db")]);
        // `ghost` matches no real module → rule unverifiable, not flagged.
        let rules = rule(Rule::ForbidEdge { from: "ghost".into(), to: "src/db".into() });
        assert!(check(&rules, &idx, Path::new(".")).is_empty());
    }

    #[test]
    fn conceptual_name_matches_real_path() {
        let idx = index(vec![edge("src/api", "src/db")]);
        let rules = rule(Rule::ForbidEdge { from: "api".into(), to: "db".into() });
        assert_eq!(check(&rules, &idx, Path::new(".")).len(), 1);
    }

    #[test]
    fn layer_depends_on_nothing() {
        let idx = index(vec![edge("src/domain", "src/infra")]);
        let rules = rule(Rule::Layer { module: "src/domain".into(), allowed: vec![] });
        assert_eq!(check(&rules, &idx, Path::new(".")).len(), 1);
    }

    #[test]
    fn layer_allows_listed_and_subtree() {
        let idx = index(vec![
            edge("src/api", "src/domain"),
            edge("src/api", "src/api/util"),
            edge("src/api", "src/db"),
        ]);
        let rules = rule(Rule::Layer {
            module: "src/api".into(),
            allowed: vec!["src/domain".into()],
        });
        let f = check(&rules, &idx, Path::new("."));
        // domain (allowed) and api/util (own subtree) pass; db is flagged.
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].code_refs, vec!["src/api -> src/db"]);
    }

    #[test]
    fn prose_forbid_edge_extracted() {
        let md = "The `controllers` layer must not import `db` directly.";
        let rules = extract_prose_rules(md, "ARCH.md");
        assert_eq!(
            rules[0].rule,
            Rule::ForbidEdge { from: "controllers".into(), to: "db".into() }
        );
        assert_eq!(rules[0].origin, "ARCH.md:1");
    }

    #[test]
    fn prose_depends_on_nothing_extracted() {
        let md = "`domain` depends on nothing.";
        let rules = extract_prose_rules(md, "ARCH.md");
        assert_eq!(rules[0].rule, Rule::Layer { module: "domain".into(), allowed: vec![] });
    }

    #[test]
    fn prose_only_depends_extracted() {
        let md = "`api` must only depend on `domain` and `util`.";
        let rules = extract_prose_rules(md, "ARCH.md");
        assert_eq!(
            rules[0].rule,
            Rule::Layer { module: "api".into(), allowed: vec!["domain".into(), "util".into()] }
        );
    }

    #[test]
    fn prose_forbid_symbol_with_except() {
        let md = "There must be no direct `os.environ` outside `config`.";
        let rules = extract_prose_rules(md, "ARCH.md");
        assert_eq!(
            rules[0].rule,
            Rule::ForbidSymbol { symbol: "os.environ".into(), except: vec!["config".into()] }
        );
    }

    #[test]
    fn quoted_example_rule_is_ignored() {
        // An author illustrating the feature in quotes is not stating a rule.
        let md = r#"- forbidden call/symbol: "no direct `os.environ` outside config""#;
        assert!(extract_prose_rules(md, "ARCH.md").is_empty());
        let md2 = r#"For example, "`api` must not import `db`" is a forbidden edge."#;
        assert!(extract_prose_rules(md2, "ARCH.md").is_empty());
    }

    #[test]
    fn bare_no_x_is_not_a_rule() {
        // "no `foo`" without a use/call signal must not become a rule.
        let md = "There is no `config` file in this layout.";
        assert!(extract_prose_rules(md, "ARCH.md").is_empty());
    }
}
