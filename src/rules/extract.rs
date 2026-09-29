//! Parsing architecture rules out of doc prose.
//!
//! Two extractors: the production [`extract_prose_rules`] (operands always
//! backtick-quoted, narrow phrasings) and the experimental, audit-only
//! [`extract_bare_rules`] (un-backticked operands kept safe by grounding + a
//! stopword denylist). Both compile to the shared [`Rule`] vocabulary.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;

use super::{Rule, SourcedRule};

/// Parse architectural rules out of one markdown doc's prose. Operands are
/// always backtick-quoted; phrasings are deliberately narrow to avoid matching
/// ordinary prose.
pub fn extract_prose_rules(markdown: &str, doc_path: &str) -> Vec<SourcedRule> {
    let mut rules = Vec::new();
    for (i, line, fenced) in logical_lines(markdown) {
        // A "rule" inside a fenced code sample is example code, not an enforced
        // architecture rule, e.g. a `// Don't call X here` comment teaching an
        // API. Only prose states rules.
        if fenced {
            continue;
        }
        // Drop double-quoted spans: an author quoting an *example* rule
        // ("no `eval`") is describing the feature, not stating an enforced rule.
        let line = quoted_re().replace_all(&line, "");
        let line = line.as_ref();
        // Hedged/conditional/historical prose isn't a stated rule: "previously,
        // `api` could not import `db`", "if `api` must not import `db`, …". The
        // whole-line check keeps mid-line markers ("e.g.", "for example")
        // suppressing the sentence they qualify even after the split below.
        if hedged(line) {
            continue;
        }
        let origin = format!("{doc_path}:{}", i + 1);
        let mut push = |rule| {
            rules.push(SourcedRule {
                rule,
                origin: origin.clone(),
            })
        };

        // Run the rule regexes per *sentence*, not per logical line, so a rule
        // can't match across a sentence boundary in a joined paragraph and a
        // hedge that leads a later sentence ("…enforced. If `api` must not
        // import `db`, use the port.") still suppresses it.
        for line in sentences(line) {
            if hedged(line) {
                continue;
            }
            // Either side may be a backticked list ("`ui`, `cli` must not import
            // `db` or `cache`"); fan out to the cross product of operands.
            for c in forbid_edge_re().captures_iter(line) {
                for from in backtick_tokens(&c[1]) {
                    for to in backtick_tokens(&c[2]) {
                        push(Rule::ForbidEdge {
                            from: from.clone(),
                            to,
                        });
                    }
                }
            }
            for c in never_edge_re().captures_iter(line) {
                for from in backtick_tokens(&c[1]) {
                    for to in backtick_tokens(&c[2]) {
                        push(Rule::ForbidEdge {
                            from: from.clone(),
                            to,
                        });
                    }
                }
            }
            // "`domain` must not transitively/indirectly reach `infra`": a path,
            // not just a direct edge. Checked before the direct verbs so the
            // transitive marker is consumed here rather than left dangling.
            for c in forbid_reach_re().captures_iter(line) {
                push(Rule::ForbidReach {
                    from: c[1].to_string(),
                    to: c[2].to_string(),
                });
            }
            // "`db` must not be imported by `api`": reverse direction (api -> db).
            for c in forbid_by_re().captures_iter(line) {
                push(Rule::ForbidEdge {
                    from: c[2].to_string(),
                    to: c[1].to_string(),
                });
            }
            // "`domain` is independent of `infra`": independence means *no path*,
            // not just no direct edge, so it compiles to symmetric ForbidReach
            // (which subsumes the direct edges).
            for c in independent_re().captures_iter(line) {
                push(Rule::ForbidReach {
                    from: c[1].to_string(),
                    to: c[2].to_string(),
                });
                push(Rule::ForbidReach {
                    from: c[2].to_string(),
                    to: c[1].to_string(),
                });
            }
            for c in depends_nothing_re().captures_iter(line) {
                push(Rule::Layer {
                    module: c[1].to_string(),
                    allowed: Vec::new(),
                });
            }
            for c in only_depends_re().captures_iter(line) {
                let allowed = backtick_tokens(&c[2]);
                if !allowed.is_empty() {
                    push(Rule::Layer {
                        module: c[1].to_string(),
                        allowed,
                    });
                }
            }
            for c in forbid_symbol_re().captures_iter(line) {
                push(Rule::ForbidSymbol {
                    symbol: c[1].to_string(),
                    except: except_modules(line),
                });
            }
        }
    }
    rules
}

/// EXPERIMENTAL (audit-only): extract dependency rules whose module operands are
/// *not* backtick-quoted, the dominant real-world phrasing ("the EVSE module
/// must not depend on the Station module", "**Repository** layer cannot
/// reference **Service**"). Backticks are normally required precisely because
/// they keep precision at 100%; here we instead lean entirely on **grounding**:
/// a bare operand is only accepted if it resolves to a real module in the graph,
/// and generic prose nouns (`modules`, `details`, `low-level`, …) are denylisted
/// so SOLID/RFC boilerplate ("high-level modules should not depend on …") cannot
/// fire even when a same-named directory happens to exist.
///
/// This is wired into `staleguard rules` (the dry-run audit) ONLY, so we can
/// measure recall/precision on real repos before letting it affect `check`.
/// Emitted operands are canonicalised to the real module segment they matched,
/// and each origin is tagged `[bare]` so the report can flag them.
pub fn extract_bare_rules(
    markdown: &str,
    doc_path: &str,
    modules: &HashSet<String>,
) -> Vec<SourcedRule> {
    let mut rules = Vec::new();
    for (i, line, _) in logical_lines(markdown) {
        let line = quoted_re().replace_all(&line, "");
        let line = line.as_ref();
        if hedged(line) {
            continue;
        }
        let origin = format!("{doc_path}:{} [bare]", i + 1);

        let mut push = |from: &str, to: &str, transitive: bool| {
            let (Some(from), Some(to)) = (
                canonical_operand(from, modules),
                canonical_operand(to, modules),
            ) else {
                return;
            };
            if from == to {
                return; // a module depending on itself is not a real rule
            }
            let rule = if transitive {
                Rule::ForbidReach { from, to }
            } else {
                Rule::ForbidEdge { from, to }
            };
            rules.push(SourcedRule {
                rule,
                origin: origin.clone(),
            });
        };

        for line in sentences(line) {
            if hedged(line) {
                continue;
            }
            for c in bare_reach_re().captures_iter(line) {
                push(&c[1], &c[2], true);
            }
            for c in bare_edge_re().captures_iter(line) {
                push(&c[1], &c[2], false);
            }
        }
    }
    rules
}

/// Generic prose nouns that are never module names: the denylist that, together
/// with grounding, keeps SOLID/RFC/security boilerplate from being read as a
/// rule. Compared case-insensitively against the bare operand token.
const BARE_STOPWORDS: &[&str] = &[
    "module",
    "modules",
    "layer",
    "layers",
    "package",
    "packages",
    "crate",
    "crates",
    "component",
    "components",
    "code",
    "library",
    "libraries",
    "class",
    "classes",
    "interface",
    "interfaces",
    "detail",
    "details",
    "abstraction",
    "abstractions",
    "concretion",
    "concretions",
    "anything",
    "nothing",
    "something",
    "them",
    "it",
    "this",
    "that",
    "these",
    "those",
    "other",
    "others",
    "any",
    "all",
    "each",
    "both",
    "one",
    "low-level",
    "high-level",
    "client",
    "clients",
    "user",
    "users",
    "entity",
    "entities",
    "file",
    "files",
    "system",
    "systems",
    "function",
    "functions",
    "method",
    "methods",
    "data",
    "type",
    "types",
    "thing",
    "things",
    "implementation",
    "implementations",
    "framework",
    "frameworks",
    "dependency",
    "dependencies",
    "runtime",
    "run-time",
    "server",
    "scope",
    "state",
    "everything",
    "concretions",
    "abstractions",
];

/// Resolve a bare prose operand to the real module segment it names, or `None`
/// if it grounds to no module (case-insensitive) or is a denylisted noun. The
/// returned string is a real path segment, so [`super::matches`] accepts it verbatim.
fn canonical_operand(op: &str, modules: &HashSet<String>) -> Option<String> {
    let op = op.trim_matches(|c: char| !c.is_alphanumeric());
    if op.is_empty() || BARE_STOPWORDS.iter().any(|w| w.eq_ignore_ascii_case(op)) {
        return None;
    }
    // Single-segment operand: match a real path segment, preserving its case.
    if !op.contains('/') {
        for m in modules {
            for seg in m.split('/') {
                if seg.eq_ignore_ascii_case(op) {
                    return Some(seg.to_string());
                }
            }
        }
        return None;
    }
    // Path-style operand (`dafny/specs`): ground by case-insensitive subtree /
    // leaf / interior match, storing the lowercased form.
    let lop = op.to_lowercase();
    for m in modules {
        let lm = m.to_lowercase();
        if lm == lop
            || lm.starts_with(&format!("{lop}/"))
            || lm.ends_with(&format!("/{lop}"))
            || lm.contains(&format!("/{lop}/"))
        {
            return Some(lop);
        }
    }
    None
}

/// Join soft-wrapped prose lines into logical lines so a rule split across a
/// hard line break ("`controllers` must not\nimport `db`") still matches.
/// Returns `(0-based first physical line, joined text, is_fenced)`. A line is
/// only appended to its predecessor when the predecessor is unfenced prose that
/// doesn't end a sentence, and the line itself doesn't start a new block
/// (blank, heading, list item, blockquote, table row, fence).
fn logical_lines(markdown: &str) -> Vec<(usize, String, bool)> {
    let fenced = crate::extract::fenced_lines(markdown);
    let mut out: Vec<(usize, String, bool)> = Vec::new();
    for (i, line) in markdown.lines().enumerate() {
        let trimmed = line.trim();
        let block_start = trimmed.is_empty()
            || trimmed.starts_with(['#', '-', '*', '+', '>', '|'])
            || trimmed.starts_with("```")
            || trimmed.starts_with("~~~")
            || list_number_re().is_match(trimmed);
        if let Some((_, prev, prev_fenced)) = out.last_mut() {
            let prev_open = !*prev_fenced
                && !prev.trim().is_empty()
                && !prev.trim_end().ends_with(['.', '!', '?', ':', ';']);
            if prev_open && !fenced[i] && !block_start {
                prev.push(' ');
                prev.push_str(trimmed);
                continue;
            }
        }
        out.push((i, line.to_string(), fenced[i]));
    }
    out
}

/// Split a logical line into sentences on `.`/`!`/`?` followed by whitespace.
/// Deliberately naive: rule operands never contain a terminator-then-space
/// (`app.core`, versions like `1.75`, and paths have no internal space), so
/// over-splitting can only *prevent* a cross-sentence match, never break a real
/// rule apart. Abbreviations like `e.g.` are already handled by the whole-line
/// hedge check before this runs.
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for m in sentence_split_re().find_iter(text) {
        out.push(text[start..m.start()].trim());
        start = m.end();
    }
    let tail = text[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out.retain(|s| !s.is_empty());
    out
}

fn sentence_split_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[.!?]\s+").unwrap())
}

/// Conditional/historical/illustrative context in which a rule-shaped sentence
/// is not a stated rule.
fn hedged(line: &str) -> bool {
    hedge_re().is_match(line)
}

fn hedge_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:previously|used\s+to|no\s+longer|historically|for\s+example|for\s+instance|e\.g\.)\b|^\s*(?:if|unless|suppose)\b",
        )
        .unwrap()
    })
}

fn list_number_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+[.)]\s").unwrap())
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

/// A backticked operand, or a comma/or/and-joined list of them
/// ("`db` or `cache`", "`ui`, `cli`"). Extracted with [`backtick_tokens`].
const OPERAND_LIST: &str = r"`[^`]+`(?:(?:\s*,\s*|\s*,?\s+(?:or|and)\s+)`[^`]+`)*";

fn forbid_edge_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"(?:(?:modules?|code|files?|classes|components?|anything)\s+(?:in|under|within)\s+)?({OPERAND_LIST})(?:\s+(?:layer|module|package|crate|component)s?)?\s+(?:(?:must|should|may|can|does|do)\s+not|cannot|can'?t)\s+(?:imports?\s+(?:anything\s+)?from|import|imports|depend\s+on|depends\s+on|use|uses|reference|references|access|accesses|touch|touches|calls?\s+into)\s+(?:the\s+)?({OPERAND_LIST})",
        ))
        .unwrap()
    })
}

fn never_edge_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r"({OPERAND_LIST})(?:\s+(?:layer|module|package|crate|component)s?)?\s+(?:(?:must|should|may|can)\s+)?never\s+(?:imports?|depends?\s+on|uses?|references?)\s+(?:the\s+)?({OPERAND_LIST})",
        ))
        .unwrap()
    })
}

/// "`X` must not be imported/used/referenced by `Y`"; captures the forbidden
/// target (1) and the dependent (2); the edge runs Y -> X.
fn forbid_reach_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"`([^`]+)`(?:\s+(?:layer|module|package|crate|component))?\s+(?:must|should|may|can|cannot)\s+not\s+(?:(?:even\s+)?(?:transitively|indirectly)\s+(?:import|imports|depend\s+on|depends\s+on|use|uses|reference|references|reach|reaches)|reach|reaches)\s+(?:the\s+)?`([^`]+)`",
        )
        .unwrap()
    })
}
fn forbid_by_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"`([^`]+)`(?:\s+(?:layer|module|package|crate|component))?\s+(?:must|should|may|can)\s+not\s+be\s+(?:imported|used|referenced|accessed|depended\s+on)\s+(?:by|from|in)\s+(?:the\s+)?`([^`]+)`",
        )
        .unwrap()
    })
}

/// "`X` is independent of `Y`" / "`X` has no dependency on `Y`": a symmetric
/// no-edge rule (both directions forbidden).
fn independent_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"`([^`]+)`(?:\s+(?:layer|module|package|crate|component))?\s+(?:is|are|stays?|remains?)\s+independent\s+(?:of|from)\s+(?:the\s+)?`([^`]+)`",
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
        Regex::new(r"`([^`]+)`(?:\s+(?:layer|module|package|crate|component))?\s+(?:(?:must|may|can|should)\s+)?only\s+(?:depends?\s+on|imports?)\s+(.*)").unwrap()
    })
}

/// Forbid-symbol phrasings, all requiring a use/call signal so a bare "no
/// `config`" in prose is not mistaken for a rule.
fn forbid_symbol_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:(?:must|should|may)\s+not\s+(?:use|call|invoke|reference)|don'?t\s+(?:use|call)|never\s+(?:use|call)|no\s+(?:direct|raw)(?:\s+(?:use|usage|calls?|reference)\s+(?:of|to))?|no\s+(?:use|usage|calls?)\s+(?:of|to))\s+`([^`]+)`",
        )
        .unwrap()
    })
}

/// EXPERIMENTAL bare-operand direct-edge pattern (no backticks). Operands are
/// captured as bare tokens (optionally **bold**, optionally `the …`, optionally
/// trailed by a noun like `layer`/`module`/`code`); grounding + the stopword
/// denylist downstream are what keep this safe. Case-insensitive.
fn bare_edge_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:the\s+)?\*{0,2}([a-z][\w.-]*(?:/[\w.-]+)*)\*{0,2}(?:\s+(?:layer|module|package|crate|component|code|library|internals?|implementations?|classes))?\s+(?:(?:must|should|may|can)\s+not|cannot|can'?t)\s+(?:import|imports|depend\s+on|depends\s+on|reference|references|access|accesses|use|uses)\s+(?:the\s+)?\*{0,2}([a-z][\w.-]*(?:/[\w.-]+)*)\*{0,2}",
        )
        .unwrap()
    })
}

/// EXPERIMENTAL bare-operand transitive pattern (no backticks). Mirrors
/// [`forbid_reach_re`] but with bare tokens.
fn bare_reach_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:the\s+)?\*{0,2}([a-z][\w.-]*(?:/[\w.-]+)*)\*{0,2}(?:\s+(?:layer|module|package|crate|component|code|library|internals?|implementations?|classes))?\s+(?:(?:must|should|may|can)\s+not|cannot|can'?t)\s+(?:(?:even\s+)?(?:transitively|indirectly)\s+(?:import|imports|depend\s+on|depends\s+on|use|uses|reference|references|reach|reaches)|reach|reaches)\s+(?:the\s+)?\*{0,2}([a-z][\w.-]*(?:/[\w.-]+)*)\*{0,2}",
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
