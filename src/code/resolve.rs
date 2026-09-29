//! Normalize raw import strings into resolved, internal repo module paths.
//!
//! The dependency edges produced by `extract` carry the import *as written*
//! (`crate::code::symbol`, `./mod`, `app.main`, `a.b.C`) in `to_module`, which
//! lives in a different namespace than the file-derived `from_module`
//! (`src/code/symbol`). Architecture-rule checks need a clean module graph, so
//! this turns each raw import into a candidate repo module path and keeps it
//! only when it matches a real module; wrong guesses simply resolve to `None`
//! and are dropped, which is what keeps the resulting graph correct.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use walkdir::WalkDir;

use crate::code::lang::{self, Language};

/// Module-resolution settings from one `tsconfig.json` / `jsconfig.json`, with
/// every path made repo-relative.
#[derive(Debug, Default)]
pub struct TsConfig {
    /// Directory holding the config (`""` at the repo root).
    dir: String,
    /// `compilerOptions.baseUrl`, if set: non-relative imports resolve from here.
    base: Option<String>,
    /// `compilerOptions.paths`: pattern (`@/*`) -> targets (`src/*`).
    paths: Vec<(String, Vec<String>)>,
}

/// Every tsconfig/jsconfig in the repo that sets `baseUrl` or `paths`.
// ponytail: `extends` isn't followed; a config without its own paths falls back
// to the nearest ancestor that has them, which covers the usual monorepo setup.
pub fn load_tsconfigs(repo_root: &Path) -> Vec<TsConfig> {
    WalkDir::new(repo_root)
        .into_iter()
        .filter_entry(|e| !lang::is_skip_dir(&e.file_name().to_string_lossy()))
        .filter_map(|e| e.ok())
        .filter(|e| {
            matches!(
                e.file_name().to_str(),
                Some("tsconfig.json") | Some("jsconfig.json")
            )
        })
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            let dir = e.path().parent()?.strip_prefix(repo_root).ok()?;
            parse_tsconfig(&text, &dir.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

fn parse_tsconfig(text: &str, dir: &str) -> Option<TsConfig> {
    let v: serde_json::Value = serde_json::from_str(&strip_jsonc(text)).ok()?;
    let opts = v.get("compilerOptions")?;
    let base_url = opts.get("baseUrl").and_then(|b| b.as_str());
    // `paths` resolve against `baseUrl` when set, else the config's own dir.
    let anchor = normalize(&format!("{dir}/{}", base_url.unwrap_or(".")));
    let paths: Vec<(String, Vec<String>)> = opts
        .get("paths")
        .and_then(|p| p.as_object())
        .map(|m| {
            m.iter()
                .map(|(pattern, targets)| {
                    let targets = targets
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|t| t.as_str())
                        .map(|t| normalize(&format!("{anchor}/{t}")))
                        .collect();
                    (pattern.clone(), targets)
                })
                .collect()
        })
        .unwrap_or_default();
    if base_url.is_none() && paths.is_empty() {
        return None;
    }
    Some(TsConfig {
        dir: dir.to_string(),
        base: base_url.map(|_| anchor),
        paths,
    })
}

/// tsconfig files are JSONC: drop `//` and `/* */` comments (outside strings)
/// and trailing commas so `serde_json` can read them.
// ponytail: the trailing-comma pass is a regex, so a string value containing
// `,}` would be mangled; no real tsconfig key or path has one.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                out.extend(chars.next());
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                while chars.next().is_some_and(|n| n != '\n') {}
                out.push('\n');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    static TRAILING_COMMA: OnceLock<Regex> = OnceLock::new();
    TRAILING_COMMA
        .get_or_init(|| Regex::new(r",(\s*[}\]])").unwrap())
        .replace_all(&out, "$1")
        .into_owned()
}

/// Resolve a raw import to an internal module path, or `None` if it points
/// outside the repo (external crate/package) or can't be resolved.
pub fn resolve_import(
    raw: &str,
    from_module: &str,
    lang: Language,
    module_set: &HashSet<String>,
    tsconfigs: &[TsConfig],
) -> Option<String> {
    let candidates = match lang {
        Language::Rust => rust_candidates(raw, from_module),
        Language::Python => dotted_candidates(raw, from_module),
        Language::Java => dotted_candidates(raw, from_module),
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            js_candidates(raw, from_module, tsconfigs)
        }
    };
    candidates.into_iter().find(|c| module_set.contains(c))
}

/// Path segments of a module path (`src/code/symbol` → `[src, code, symbol]`).
fn parts(module: &str) -> Vec<&str> {
    module.split('/').filter(|s| !s.is_empty()).collect()
}

/// Collapse `.`, `..`, and empty segments: `src/a/../b/./c` -> `src/b/c`.
fn normalize(path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
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
            // module declared with `mod x;` in this file; resolve relative to
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

/// JS/TS specifiers: `./mod` / `../lib/x` resolve against the importer's
/// directory; non-relative ones through the nearest tsconfig's `paths` and
/// `baseUrl`. Anything left (`react`) is external. Each target is also tried as
/// a directory import (`./lib` -> `lib/index`).
fn js_candidates(raw: &str, from_module: &str, tsconfigs: &[TsConfig]) -> Vec<String> {
    let mut targets = Vec::new();
    if raw.starts_with('.') {
        let dir = from_module.rsplit_once('/').map_or("", |(d, _)| d);
        targets.push(format!("{dir}/{raw}"));
    } else if let Some(cfg) = nearest_tsconfig(from_module, tsconfigs) {
        for (pattern, subs) in &cfg.paths {
            let rest = match pattern.strip_suffix('*') {
                Some(prefix) => raw.strip_prefix(prefix),
                None => (raw == pattern).then_some(""),
            };
            if let Some(rest) = rest {
                targets.extend(subs.iter().map(|t| t.replace('*', rest)));
            }
        }
        if let Some(base) = &cfg.base {
            targets.push(format!("{base}/{raw}"));
        }
    }
    targets
        .iter()
        .map(|t| normalize(strip_js_ext(t)))
        .filter(|t| !t.is_empty())
        .flat_map(|t| [format!("{t}/index"), t])
        .collect()
}

/// Drop a JS/TS source extension (only a real one, so `./user.service` keeps its
/// dot; ESM-style `./x.js` still maps to `x.ts`).
fn strip_js_ext(path: &str) -> &str {
    const EXTS: &[&str] = &[".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"];
    EXTS.iter()
        .find_map(|e| path.strip_suffix(e))
        .unwrap_or(path)
}

/// The tsconfig governing `from_module`: the deepest one whose directory
/// contains it.
fn nearest_tsconfig<'a>(from_module: &str, tsconfigs: &'a [TsConfig]) -> Option<&'a TsConfig> {
    tsconfigs
        .iter()
        .filter(|c| c.dir.is_empty() || from_module.starts_with(&format!("{}/", c.dir)))
        .max_by_key(|c| c.dir.len())
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
            resolve_import("crate::code::symbol", "src/verify", Language::Rust, &m, &[]).as_deref(),
            Some("src/code/symbol")
        );
        // Bare local path without `crate::`.
        assert_eq!(
            resolve_import("extract::PathClaim", "src/main", Language::Rust, &m, &[]).as_deref(),
            Some("src/extract")
        );
    }

    #[test]
    fn rust_super_and_self() {
        let m = set(&["src/code", "src/code/symbol"]);
        assert_eq!(
            resolve_import("super::symbol", "src/code/extract", Language::Rust, &m, &[]).as_deref(),
            Some("src/code/symbol")
        );
    }

    #[test]
    fn rust_external_is_dropped() {
        let m = set(&["src/main"]);
        assert!(resolve_import("std::fmt", "src/main", Language::Rust, &m, &[]).is_none());
        assert!(resolve_import("regex::Regex", "src/main", Language::Rust, &m, &[]).is_none());
    }

    #[test]
    fn python_dotted_resolves_and_external_dropped() {
        let m = set(&["app/main", "app/util"]);
        assert_eq!(
            resolve_import("app.main", "app/cli", Language::Python, &m, &[]).as_deref(),
            Some("app/main")
        );
        assert!(resolve_import("os", "app/cli", Language::Python, &m, &[]).is_none());
    }

    #[test]
    fn js_relative_resolves_and_bare_dropped() {
        let m = set(&["src/mod", "lib/x"]);
        assert_eq!(
            resolve_import("./mod", "src/a", Language::JavaScript, &m, &[]).as_deref(),
            Some("src/mod")
        );
        assert_eq!(
            resolve_import("../lib/x", "src/a", Language::TypeScript, &m, &[]).as_deref(),
            Some("lib/x")
        );
        assert!(resolve_import("react", "src/a", Language::JavaScript, &m, &[]).is_none());
    }

    #[test]
    fn js_dotted_names_index_dirs_and_extensions() {
        let m = set(&["src/user.service", "src/lib/index", "src/ui/z"]);
        let r = |raw| resolve_import(raw, "src/app", Language::TypeScript, &m, &[]);
        assert_eq!(r("./user.service").as_deref(), Some("src/user.service"));
        assert_eq!(r("./user.service.js").as_deref(), Some("src/user.service"));
        assert_eq!(r("./lib").as_deref(), Some("src/lib/index"));
        assert_eq!(r("./ui/z.ts").as_deref(), Some("src/ui/z"));
    }

    #[test]
    fn tsconfig_paths_and_base_url_resolve() {
        let cfg = parse_tsconfig(
            r##"{
              // JSONC: comments and trailing commas are allowed
              "compilerOptions": {
                "baseUrl": ".", /* repo-relative */
                "paths": { "@/*": ["src/*"], "#config": ["src/config/index.ts"], },
              },
            }"##,
            "apps/web",
        )
        .unwrap();
        let m = set(&[
            "apps/web/src/lib/keys",
            "apps/web/src/config/index",
            "apps/web/lib/util",
        ]);
        let r = |raw| {
            resolve_import(
                raw,
                "apps/web/src/app",
                Language::TypeScript,
                &m,
                std::slice::from_ref(&cfg),
            )
        };
        assert_eq!(r("@/lib/keys").as_deref(), Some("apps/web/src/lib/keys"));
        assert_eq!(r("#config").as_deref(), Some("apps/web/src/config/index"));
        assert_eq!(r("lib/util").as_deref(), Some("apps/web/lib/util"));
        assert!(r("react").is_none());
        // A module outside the config's directory doesn't use its aliases.
        assert!(resolve_import(
            "@/lib/keys",
            "other/a",
            Language::TypeScript,
            &m,
            std::slice::from_ref(&cfg)
        )
        .is_none());
    }

    #[test]
    fn java_package_resolves() {
        let m = set(&["a/b", "a/b/C"]);
        assert_eq!(
            resolve_import("a.b.C", "a/Main", Language::Java, &m, &[]).as_deref(),
            Some("a/b/C")
        );
    }
}
