//! Normalize raw import strings into resolved, internal repo module paths.
//!
//! The dependency edges produced by `extract` carry the import *as written*
//! (`crate::code::symbol`, `./mod`, `app.main`, `a.b.C`) in `to_module`, which
//! lives in a different namespace than the file-derived `from_module`
//! (`src/code/symbol`). Architecture-rule checks need a clean module graph, so
//! this turns each raw import into a candidate repo module path and keeps it
//! only when it matches a real module — wrong guesses simply resolve to `None`
//! and are dropped, which is what keeps the resulting graph correct.

use std::collections::HashSet;

use crate::code::lang::Language;

/// Resolve a raw import to an internal module path, or `None` if it points
/// outside the repo (external crate/package) or can't be resolved.
pub fn resolve_import(
    raw: &str,
    from_module: &str,
    lang: Language,
    module_set: &HashSet<String>,
) -> Option<String> {
    let candidates = match lang {
        Language::Rust => rust_candidates(raw, from_module),
        Language::Python => dotted_candidates(raw, from_module),
        Language::Java => dotted_candidates(raw, from_module),
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            js_candidates(raw, from_module)
        }
    };
    candidates.into_iter().find(|c| module_set.contains(c))
}

/// Path segments of a module path (`src/code/symbol` → `[src, code, symbol]`).
fn parts(module: &str) -> Vec<&str> {
    module.split('/').filter(|s| !s.is_empty()).collect()
}

/// Join non-empty segments into a module path.
fn join(segs: &[String]) -> String {
    segs.iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("/")
}

/// Add `path` and `path` minus its last segment (the tail may be a symbol, not a
/// module) to `out`.
fn push_with_trimmed(segs: &[String], out: &mut Vec<String>) {
    if segs.is_empty() {
        return;
    }
    out.push(join(segs));
    if segs.len() > 1 {
        out.push(join(&segs[..segs.len() - 1]));
    }
}

/// `crate::a::b`, `self::a`, `super::a`, or a bare `a::b` path, resolved against
/// the importing module and the crate root (the first segment of `from_module`).
fn rust_candidates(raw: &str, from_module: &str) -> Vec<String> {
    let segs: Vec<String> = raw.split("::").map(str::to_string).collect();
    let from = parts(from_module);
    let root = from.first().map(|s| s.to_string());
    let mut out = Vec::new();

    let owned = |segs: &[&str]| -> Vec<String> { segs.iter().map(|s| s.to_string()).collect() };
    let parent: Vec<&str> = if from.len() > 1 {
        from[..from.len() - 1].to_vec()
    } else {
        from.clone()
    };

    match segs.first().map(String::as_str) {
        Some("crate") => {
            let mut p = root.clone().into_iter().collect::<Vec<_>>();
            p.extend_from_slice(&segs[1..]);
            push_with_trimmed(&p, &mut out);
        }
        Some("self") => {
            let mut p = owned(&from);
            p.extend_from_slice(&segs[1..]);
            push_with_trimmed(&p, &mut out);
        }
        Some("super") => {
            let mut p = owned(&parent);
            p.extend_from_slice(&segs[1..]);
            push_with_trimmed(&p, &mut out);
        }
        _ => {
            // A bare path (e.g. `extract::RawRef`) is usually a sibling/child
            // module declared with `mod x;` in this file — resolve relative to
            // the importing module's directory first, then the crate root, then
            // as written.
            if from.len() > 1 {
                let mut sib = owned(&parent);
                sib.extend_from_slice(&segs);
                push_with_trimmed(&sib, &mut out);
            }
            if let Some(r) = &root {
                let mut rooted = vec![r.clone()];
                rooted.extend_from_slice(&segs);
                push_with_trimmed(&rooted, &mut out);
            }
            push_with_trimmed(&segs, &mut out);
        }
    }
    out
}

/// Dotted module paths (`app.main`, `a.b.C`), resolved as written and relative
/// to the importing module's package.
fn dotted_candidates(raw: &str, from_module: &str) -> Vec<String> {
    let segs: Vec<String> = raw
        .trim_start_matches('.')
        .split('.')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let mut out = Vec::new();
    push_with_trimmed(&segs, &mut out);

    // Relative to the importing package (parent dir of `from_module`).
    let from = parts(from_module);
    if from.len() > 1 {
        let mut rel: Vec<String> = from[..from.len() - 1]
            .iter()
            .map(|s| s.to_string())
            .collect();
        rel.extend_from_slice(&segs);
        push_with_trimmed(&rel, &mut out);
    }
    out
}

/// JS/TS specifiers: `./mod` / `../lib/x` resolved against the importer's
/// directory; bare specifiers (`react`) are external.
fn js_candidates(raw: &str, from_module: &str) -> Vec<String> {
    if !raw.starts_with('.') {
        return Vec::new();
    }
    // Directory of the importing module.
    let from = parts(from_module);
    let mut stack: Vec<String> = if from.len() > 1 {
        from[..from.len() - 1]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        Vec::new()
    };
    for seg in raw.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => {
                // Drop a trailing file extension on the final segment.
                let trimmed = other.split('.').next().unwrap_or(other);
                stack.push(trimmed.to_string());
            }
        }
    }
    if stack.is_empty() {
        Vec::new()
    } else {
        vec![join(&stack)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rust_crate_path_resolves() {
        let m = set(&["src/code/symbol", "src/extract", "src/verify"]);
        assert_eq!(
            resolve_import("crate::code::symbol", "src/verify", Language::Rust, &m).as_deref(),
            Some("src/code/symbol")
        );
        // Bare local path without `crate::`.
        assert_eq!(
            resolve_import("extract::PathClaim", "src/main", Language::Rust, &m).as_deref(),
            Some("src/extract")
        );
    }

    #[test]
    fn rust_super_and_self() {
        let m = set(&["src/code", "src/code/symbol"]);
        assert_eq!(
            resolve_import("super::symbol", "src/code/extract", Language::Rust, &m).as_deref(),
            Some("src/code/symbol")
        );
    }

    #[test]
    fn rust_external_is_dropped() {
        let m = set(&["src/main"]);
        assert!(resolve_import("std::fmt", "src/main", Language::Rust, &m).is_none());
        assert!(resolve_import("regex::Regex", "src/main", Language::Rust, &m).is_none());
    }

    #[test]
    fn python_dotted_resolves_and_external_dropped() {
        let m = set(&["app/main", "app/util"]);
        assert_eq!(
            resolve_import("app.main", "app/cli", Language::Python, &m).as_deref(),
            Some("app/main")
        );
        assert!(resolve_import("os", "app/cli", Language::Python, &m).is_none());
    }

    #[test]
    fn js_relative_resolves_and_bare_dropped() {
        let m = set(&["src/mod", "lib/x"]);
        assert_eq!(
            resolve_import("./mod", "src/a", Language::JavaScript, &m).as_deref(),
            Some("src/mod")
        );
        assert_eq!(
            resolve_import("../lib/x", "src/a", Language::TypeScript, &m).as_deref(),
            Some("lib/x")
        );
        assert!(resolve_import("react", "src/a", Language::JavaScript, &m).is_none());
    }

    #[test]
    fn java_package_resolves() {
        let m = set(&["a/b", "a/b/C"]);
        assert_eq!(
            resolve_import("a.b.C", "a/Main", Language::Java, &m).as_deref(),
            Some("a/b/C")
        );
    }
}
