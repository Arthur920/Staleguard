//! Verify shell commands quoted in docs against the repo's build
//! manifests. An `npm run` / `make` / `cargo --bin` invocation that names a
//! script, target, or binary the repo doesn't declare is `stale`.
//!
//! Zero false positives by construction: a command is only checked against a
//! registry that actually exists. No `package.json` ⇒ npm commands are never
//! flagged (we can't know the scripts); likewise for Makefile and Cargo.toml.
//! Bare lifecycle commands (`npm test`, `cargo build`, `make` with no target)
//! have built-in defaults, so they are never flagged either.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;

use crate::claim::Provenance;
use crate::findings::{Finding, Verdict};

/// The valid invocation targets the repo declares, per tool. `None` means "no
/// manifest for this tool"; claims for it are left unchecked.
#[derive(Debug, Default, Clone)]
pub struct Manifests {
    npm_scripts: Option<HashSet<String>>,
    /// Dependency names from the nearest and the root `package.json`: `pnpm tsx`
    /// / `yarn vitest` run a dependency's binary, not a script.
    npm_dep_bins: HashSet<String>,
    make_targets: Option<HashSet<String>>,
    cargo_bins: Option<HashSet<String>>,
    /// Names by which *this* project's own CLI is invoked (cargo bins + npm
    /// package/bin names). Used by the flag check to scope `--flags` to our CLI.
    project_bins: HashSet<String>,
}

impl Manifests {
    /// Load every manifest from `root` itself. Thin wrapper over
    /// [`Manifests::load_nearest`] for tests.
    #[cfg(test)]
    pub fn load(root: &Path) -> Manifests {
        Manifests::load_nearest(root, root)
    }

    /// Resolve each manifest from the *nearest* ancestor of `start` (up to and
    /// including `root`) that declares it, so a `pydantic-core/README.md` is
    /// checked against `pydantic-core/Makefile`, not just the repo-root one. Each
    /// manifest type is located independently (the nearest `Makefile` and the
    /// nearest `Cargo.toml` can sit in different directories). Without this, every
    /// command in a sub-project's docs that hit a sibling manifest was a false
    /// "target does not exist" finding.
    pub fn load_nearest(start: &Path, root: &Path) -> Manifests {
        let npm_dir = find_up(start, root, &["package.json"]);
        let npm = npm_dir.as_deref().and_then(load_package_json);
        let mut npm_dep_bins = HashSet::new();
        for dir in npm_dir.iter().map(|d| d.as_path()).chain([root]) {
            npm_dep_bins.extend(load_dependency_names(dir));
        }
        let cargo_bins = find_up(start, root, &["Cargo.toml"]).and_then(|d| load_cargo_bins(&d));
        let make_targets = find_up(start, root, &["Makefile", "makefile", "GNUmakefile"])
            .and_then(|d| load_make_targets(&d));

        let mut project_bins = HashSet::new();
        if let Some((_, ref bins)) = npm {
            project_bins.extend(bins.iter().cloned());
        }
        if let Some(ref bins) = cargo_bins {
            project_bins.extend(bins.iter().cloned());
        }

        Manifests {
            npm_scripts: npm.map(|(scripts, _)| scripts),
            npm_dep_bins,
            make_targets,
            cargo_bins,
            project_bins,
        }
    }

    /// Names this project's CLI is invoked by. Empty if nothing declares a bin.
    pub fn project_bins(&self) -> &HashSet<String> {
        &self.project_bins
    }
}

/// Walk up from `start` to `root` (inclusive) and return the first directory that
/// contains any of `names`. `start` may be a file or a directory; the search
/// begins at its containing directory.
fn find_up(start: &Path, root: &Path, names: &[&str]) -> Option<std::path::PathBuf> {
    let mut dir = if start.is_dir() {
        Some(start)
    } else {
        start.parent()
    };
    while let Some(d) = dir {
        if names.iter().any(|n| d.join(n).exists()) {
            return Some(d.to_path_buf());
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }
    None
}

// ---- manifest parsing -----------------------------------------------------

/// Returns `(script names, binary names)` from `package.json`, if present and
/// parseable. Scripts is `None`-collapsed by the caller when there is no
/// `scripts` table; binary names come from `bin` (and `name`, the implicit bin).
fn load_package_json(root: &Path) -> Option<(HashSet<String>, HashSet<String>)> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;

    let scripts: HashSet<String> = json
        .get("scripts")
        .and_then(|s| s.as_object())
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();

    let mut bins = HashSet::new();
    match json.get("bin") {
        Some(serde_json::Value::String(_)) => {
            if let Some(name) = json.get("name").and_then(|n| n.as_str()) {
                bins.insert(name.to_string());
            }
        }
        Some(serde_json::Value::Object(o)) => bins.extend(o.keys().cloned()),
        _ => {
            if let Some(name) = json.get("name").and_then(|n| n.as_str()) {
                bins.insert(name.to_string());
            }
        }
    }

    Some((scripts, bins))
}

/// Names a dependency's binary is likely invoked by: every `dependencies` /
/// `devDependencies` key, plus the unscoped tail of a scoped one
/// (`@biomejs/biome` -> `biome`).
// ponytail: guesses bin names from package names rather than reading each
// installed package's `bin`; covers tsx/vitest/prisma/eslint-style tools.
fn load_dependency_names(dir: &Path) -> HashSet<String> {
    let Some(json) = std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    else {
        return HashSet::new();
    };
    let mut out = HashSet::new();
    for key in ["dependencies", "devDependencies"] {
        for name in json
            .get(key)
            .and_then(|d| d.as_object())
            .into_iter()
            .flat_map(|d| d.keys())
        {
            out.insert(name.clone());
            if let Some((_, tail)) = name.rsplit_once('/') {
                out.insert(tail.to_string());
            }
            out.extend(installed_bins(dir, name));
            if let Some((_, bins)) = KNOWN_BINS.iter().find(|(pkg, _)| pkg == name) {
                out.extend(bins.iter().map(|b| b.to_string()));
            }
        }
    }
    out
}

/// Common packages whose binary name differs from the package name, for repos
/// checked without `node_modules` (CI checkouts, fresh clones).
const KNOWN_BINS: &[(&str, &[&str])] = &[
    ("@changesets/cli", &["changeset"]),
    ("typescript", &["tsc"]),
    ("@playwright/test", &["playwright"]),
    ("@nestjs/cli", &["nest"]),
    ("@angular/cli", &["ng"]),
    ("npm-run-all", &["run-s", "run-p"]),
    ("npm-run-all2", &["run-s", "run-p"]),
    ("@biomejs/biome", &["biome"]),
    ("@graphql-codegen/cli", &["graphql-codegen"]),
];

/// Binary names an installed dependency declares (`@changesets/cli` ships
/// `changeset`). ponytail: only sees installed deps; without `node_modules`
/// the package name is the only guess.
fn installed_bins(dir: &Path, dep: &str) -> Vec<String> {
    let Some(json) =
        std::fs::read_to_string(dir.join("node_modules").join(dep).join("package.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    else {
        return Vec::new();
    };
    match json.get("bin") {
        Some(serde_json::Value::Object(bins)) => bins.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// GNU-make target names: the labels left of `:` on rule lines (skipping
/// recipe lines, which start with a tab, and `:=`/`=` variable assignments),
/// plus the prerequisites listed on a `.PHONY:` line.
fn load_make_targets(root: &Path) -> Option<HashSet<String>> {
    let text = ["Makefile", "makefile", "GNUmakefile"]
        .iter()
        .find_map(|n| std::fs::read_to_string(root.join(n)).ok())?;

    let mut targets = HashSet::new();
    for line in text.lines() {
        let Some(caps) = make_rule_re().captures(line) else {
            continue;
        };
        let names: Vec<&str> = caps[1].split_whitespace().collect();
        let is_phony = names.first() == Some(&".PHONY");
        for n in &names {
            targets.insert((*n).to_string());
        }
        if is_phony {
            // `.PHONY: build test` declares build/test as targets.
            for dep in caps[2].split_whitespace() {
                targets.insert(dep.to_string());
            }
        }
    }
    Some(targets)
}

/// Binary names a `cargo run/build --bin <name>` could validly reference:
/// every `[[bin]]` `name`, plus the package name when a default bin exists
/// (`src/main.rs`), plus `src/bin/*.rs` stems. Regex/line parsing of the
/// manifest keeps the deterministic build free of a TOML dependency.
fn load_cargo_bins(root: &Path) -> Option<HashSet<String>> {
    let text = std::fs::read_to_string(root.join("Cargo.toml")).ok()?;

    let mut bins = HashSet::new();
    let mut section = "";
    let mut pkg_name: Option<String> = None;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            section = if l.starts_with("[[bin]]") {
                "bin"
            } else if l.starts_with("[package]") {
                "package"
            } else {
                "other"
            };
            continue;
        }
        if let Some(name) = toml_name(l) {
            match section {
                "bin" => {
                    bins.insert(name);
                }
                "package" => pkg_name = Some(name),
                _ => {}
            }
        }
    }

    // The package name is a valid bin only if a default binary exists.
    if let Some(name) = pkg_name {
        if root.join("src/main.rs").exists() {
            bins.insert(name);
        }
    }
    if let Ok(entries) = std::fs::read_dir(root.join("src/bin")) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    bins.insert(stem.to_string());
                }
            }
        }
    }

    Some(bins)
}

/// Parse `name = "value"` (a TOML string assignment) into the value.
fn toml_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("name")?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let inner = rest.strip_prefix('"')?;
    let end = inner.find('"')?;
    Some(inner[..end].to_string())
}

// ---- command extraction ---------------------------------------------------

/// Every command-ish string found in docs, as `(line_no, command)`. Lines
/// inside fenced code blocks are taken whole; outside fences only the contents
/// of `backtick` spans are taken. Compound commands are split on `&&`, `||`,
/// `;`, and `|`, and a leading `$ ` shell prompt is stripped. Shared with the
/// flag check in [`crate::config`].
pub fn command_lines(markdown: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (i, line) in markdown.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        let lineno = i + 1;
        if in_fence {
            push_subcommands(line, lineno, &mut out);
        } else {
            for cap in inline_code_re().captures_iter(line) {
                push_subcommands(&cap[1], lineno, &mut out);
            }
        }
    }
    out
}

fn push_subcommands(raw: &str, lineno: usize, out: &mut Vec<(usize, String)>) {
    // Drop a trailing shell comment: `npm run seed  # via the root alias`.
    let raw = raw.split(" #").next().unwrap_or(raw);
    for part in chain_split_re().split(raw) {
        let mut cmd = part.trim();
        cmd = cmd.strip_prefix("$ ").unwrap_or(cmd).trim();
        if !cmd.is_empty() {
            out.push((lineno, cmd.to_string()));
        }
    }
}

/// What kind of registry a parsed command resolves against.
enum Target<'a> {
    NpmScript(&'a str),
    /// `pnpm x` / `yarn x`: a script, or else a dependency's binary.
    RunnerScript(&'a str),
    Make(&'a str),
    CargoBin(&'a str),
}

/// Check every command claim in `markdown` against the manifests. A command
/// that resolves to a declared target yields a `Supported` claim anchored to
/// the manifest that declares it; one that names a missing target yields a
/// `Stale` claim. Commands with no manifest to check against are skipped.
///
/// `repo_scripts` is every script any `package.json` in the repo defines: a
/// root README's "Frontend (`packages/frontend/`)" section runs scripts from
/// that package, so a script is only missing if no package defines it.
pub fn check(
    markdown: &str,
    doc_path: &str,
    m: &Manifests,
    repo_scripts: &HashSet<String>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (line, cmd) in command_lines(markdown) {
        let toks: Vec<&str> = cmd.split_whitespace().collect();
        let Some(target) = classify(&toks) else {
            continue;
        };
        let (kind, name, registry, manifest) = match target {
            Target::NpmScript(n) | Target::RunnerScript(n) => {
                ("script", n, m.npm_scripts.as_ref(), "package.json")
            }
            Target::Make(n) => ("make target", n, m.make_targets.as_ref(), "Makefile"),
            Target::CargoBin(n) => ("cargo binary", n, m.cargo_bins.as_ref(), "Cargo.toml"),
        };
        // Only act when the registry exists (else we can't know the targets).
        let Some(registry) = registry else { continue };
        let doc_ref = format!("{doc_path}:{line}");
        let prov = Provenance::path(manifest);
        let dep_bin = matches!(target, Target::RunnerScript(_)) && m.npm_dep_bins.contains(name);
        let npm = matches!(target, Target::NpmScript(_) | Target::RunnerScript(_));
        if registry.contains(name) || dep_bin || (npm && repo_scripts.contains(name)) {
            findings.push(Finding::supported(format!("runs `{cmd}`"), doc_ref, prov));
        } else {
            findings.push(
                Finding::problem(
                    Verdict::Stale,
                    format!("runs `{cmd}`"),
                    doc_ref,
                    format!("{kind} `{name}` is not defined"),
                )
                .anchored(prov),
            );
        }
    }
    findings
}

/// Resolve a tokenized command to the registry entry it depends on, if any.
fn classify<'a>(toks: &[&'a str]) -> Option<Target<'a>> {
    let first = *toks.first()?;
    // ponytail: workspace-targeted runs are skipped, not resolved against the
    // workspace's package.json; resolve them if they turn out to rot often.
    if matches!(first, "npm" | "pnpm" | "yarn") && toks.iter().any(|t| is_workspace_flag(t)) {
        return None;
    }
    match first {
        "npm" => {
            // Only the explicit `npm run <script>` form; bare `npm test` etc.
            // have npm built-in defaults and are not script-guaranteed.
            if toks.get(1) == Some(&"run") {
                toks[2..]
                    .iter()
                    .find(|t| !t.starts_with('-'))
                    .map(|s| Target::NpmScript(s))
            } else {
                None
            }
        }
        "pnpm" | "yarn" => package_runner_script(&toks[1..]),
        "cargo" => {
            // `cargo new --bin <name>` / `cargo init --bin` *create* a project:
            // `--bin` is a boolean here, and the trailing name is the new crate,
            // not a target this repo must already define.
            if matches!(toks.get(1), Some(&"new") | Some(&"init")) {
                return None;
            }
            toks.iter()
                .position(|t| *t == "--bin")
                .and_then(|i| toks.get(i + 1))
                .map(|s| Target::CargoBin(s))
                .or_else(|| {
                    toks.iter()
                        .find_map(|t| t.strip_prefix("--bin="))
                        .map(Target::CargoBin)
                })
        }
        "make" => make_target(&toks[1..]).map(Target::Make),
        _ => None,
    }
}

/// Flags that point npm/pnpm/yarn at another package's `package.json`
/// (`npm run x -w pkg`, `pnpm --filter pkg x`, `yarn workspace pkg x`).
fn is_workspace_flag(t: &str) -> bool {
    matches!(
        t,
        "-w" | "--workspace"
            | "--filter"
            | "-F"
            | "-C"
            | "--dir"
            | "--prefix"
            | "workspace"
            | "workspaces"
    ) || ["--workspace=", "--filter=", "--dir=", "--prefix="]
        .iter()
        .any(|p| t.starts_with(p))
}

/// pnpm/yarn run scripts directly (`yarn build` == `yarn run build`). Resolve
/// the script name unless the first arg is a built-in subcommand or a flag.
fn package_runner_script<'a>(args: &[&'a str]) -> Option<Target<'a>> {
    let first = args.first()?;
    if *first == "run" {
        return args.get(1).map(|s| Target::NpmScript(s));
    }
    if first.starts_with('-') || PM_BUILTINS.contains(first) {
        return None;
    }
    Some(Target::RunnerScript(first))
}

/// First positional argument to `make` (its goal), skipping flags and the
/// values of `-C`/`-f`/`-j`/etc., and `VAR=value` overrides.
fn make_target<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if MAKE_VALUE_FLAGS.contains(a) {
            skip_next = true;
            continue;
        }
        if a.starts_with('-') || a.contains('=') {
            continue;
        }
        return Some(a);
    }
    None
}

/// pnpm/yarn subcommands that are not user scripts. `build`, `dev`, and `test`
/// are deliberately absent: `pnpm build` just runs the `build` script and fails
/// without one. `start` stays: it falls back to `node server.js`.
const PM_BUILTINS: &[&str] = &[
    "add", "remove", "rm", "install", "i", "ci", "init", "create", "up", "update", "upgrade",
    "why", "link", "unlink", "dlx", "exec", "publish", "pack", "info", "view", "list", "ls",
    "audit", "outdated", "global", "set", "get", "config", "import", "store", "patch", "prune",
    "rebuild", "start", "stop", "restart", "version", "login", "logout", "whoami", "cache",
    "dedupe", "fund", "help", "x", "node",
];

/// make flags that take a separate value argument.
const MAKE_VALUE_FLAGS: &[&str] = &["-C", "-f", "-j", "-I", "-o", "-W", "-l", "--directory"];

fn inline_code_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([^`]+)`").unwrap())
}

fn chain_split_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"&&|\|\||;|\|").unwrap())
}

/// A make rule line: target name(s) before `:`, prerequisites after, excluding
/// recipe lines (leading tab) and `:=`/`=` assignments.
fn make_rule_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([A-Za-z0-9_.%/ -]+):(?:[^=]|$)(.*)$").unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("staleguard-cmd-{tag}-{nanos}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn npm_run_missing_script_is_flagged() {
        let dir = scratch("npm");
        fs::write(
            dir.join("package.json"),
            r#"{"name":"x","scripts":{"build":"tsc"}}"#,
        )
        .unwrap();
        let m = Manifests::load(&dir);
        let md = "Run `npm run build` then `npm run deploy`.";
        let flagged: Vec<String> = check(md, "README.md", &m, &HashSet::new())
            .iter()
            .filter(|f| f.verdict.is_reportable())
            .map(|f| f.detail.clone())
            .collect();
        assert!(flagged.iter().any(|d| d.contains("deploy")));
        assert!(!flagged.iter().any(|d| d.contains("`build`")));
    }

    #[test]
    fn workspace_targeted_runs_are_skipped() {
        let dir = scratch("ws");
        fs::write(
            dir.join("package.json"),
            r#"{"name":"x","scripts":{"build":"tsc"}}"#,
        )
        .unwrap();
        let m = Manifests::load(&dir);
        let md = "`npm run eval -w packages/backend` `npm run --workspace=backend eval` \
                  `pnpm --filter api seed` `yarn workspace api seed` `npm run --silent gone`";
        let flagged: Vec<String> = check(md, "README.md", &m, &HashSet::new())
            .iter()
            .filter(|f| f.verdict.is_reportable())
            .map(|f| f.detail.clone())
            .collect();
        assert_eq!(flagged.len(), 1, "{flagged:?}");
        assert!(flagged[0].contains("`gone`"));
    }

    #[test]
    fn script_defined_by_another_package_is_not_missing() {
        let dir = scratch("reposcripts");
        fs::write(dir.join("package.json"), r#"{"name":"x","scripts":{}}"#).unwrap();
        let m = Manifests::load(&dir);
        let repo: HashSet<String> = ["preview".to_string()].into();
        let md = "`npm run preview` \n```sh\nnpm run gone   # a comment\n```";
        let flagged: Vec<_> = check(md, "README.md", &m, &repo)
            .into_iter()
            .filter(|f| f.verdict.is_reportable())
            .collect();
        assert_eq!(flagged.len(), 1);
        assert!(flagged[0].detail.contains("`gone`"));
        assert!(!flagged[0].detail.contains('#'));
    }

    #[test]
    fn installed_dependency_bin_is_runnable() {
        let dir = scratch("depbin");
        fs::write(
            dir.join("package.json"),
            r#"{"name":"x","scripts":{},"devDependencies":{"@changesets/cli":"2"}}"#,
        )
        .unwrap();
        let pkg = dir.join("node_modules/@changesets/cli");
        fs::create_dir_all(&pkg).unwrap();
        fs::write(
            pkg.join("package.json"),
            r#"{"bin":{"changeset":"bin.js"}}"#,
        )
        .unwrap();
        let m = Manifests::load(&dir);
        let found = check(
            "`pnpm changeset` `pnpm nope`",
            "README.md",
            &m,
            &HashSet::new(),
        );
        let flagged: Vec<_> = found.iter().filter(|f| f.verdict.is_reportable()).collect();
        assert_eq!(flagged.len(), 1);
        assert!(flagged[0].detail.contains("`nope`"));
    }

    #[test]
    fn no_package_json_means_no_npm_findings() {
        let dir = scratch("nopkg");
        let m = Manifests::load(&dir);
        assert!(check("`npm run anything`", "README.md", &m, &HashSet::new()).is_empty());
    }

    #[test]
    fn make_target_checked_against_makefile() {
        let dir = scratch("make");
        fs::write(
            dir.join("Makefile"),
            "build:\n\tcargo build\n.PHONY: build test\n",
        )
        .unwrap();
        let m = Manifests::load(&dir);
        let md = "```sh\nmake build\nmake test\nmake nope\n```";
        let flagged: Vec<String> = check(md, "README.md", &m, &HashSet::new())
            .iter()
            .filter(|f| f.verdict.is_reportable())
            .map(|f| f.detail.clone())
            .collect();
        assert_eq!(flagged.len(), 1);
        assert!(flagged[0].contains("nope"));
    }

    #[test]
    fn make_target_resolved_from_nearest_makefile() {
        // A sub-project Makefile must be found from a doc inside that subtree,
        // not just the repo root (which here has no Makefile at all).
        let dir = scratch("nearest");
        let sub = dir.join("subproj");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("Makefile"), "build-dev:\n\tcargo build\n").unwrap();
        let m = Manifests::load_nearest(&sub, &dir);
        let md = "```sh\nmake build-dev\nmake ghost\n```";
        let flagged: Vec<String> = check(md, "subproj/README.md", &m, &HashSet::new())
            .iter()
            .filter(|f| f.verdict.is_reportable())
            .map(|f| f.detail.clone())
            .collect();
        assert_eq!(flagged.len(), 1);
        assert!(flagged[0].contains("ghost"));
    }

    #[test]
    fn cargo_bin_checked_against_manifest() {
        let dir = scratch("cargo");
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"staleguard\"\n\n[[bin]]\nname = \"staleguard\"\n",
        )
        .unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        let m = Manifests::load(&dir);
        let md = "`cargo run --bin staleguard` works; `cargo run --bin ghost` does not.";
        let flagged: Vec<String> = check(md, "README.md", &m, &HashSet::new())
            .iter()
            .filter(|f| f.verdict.is_reportable())
            .map(|f| f.detail.clone())
            .collect();
        assert_eq!(flagged.len(), 1);
        assert!(flagged[0].contains("ghost"));
    }

    #[test]
    fn cargo_new_does_not_flag_created_project() {
        let dir = scratch("cargonew");
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let m = Manifests::load(&dir);
        // `cargo new --bin foo` creates a project; `foo` is not a target here.
        let md = "Run `cargo new --bin mdbook-wordcount` to start.";
        assert!(check(md, "README.md", &m, &HashSet::new()).is_empty());
    }

    #[test]
    fn bare_lifecycle_commands_are_not_flagged() {
        let dir = scratch("life");
        fs::write(dir.join("package.json"), r#"{"name":"x","scripts":{}}"#).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let m = Manifests::load(&dir);
        // npm test / cargo build / make (default) carry no named target.
        assert!(check(
            "`npm test` and `cargo build`",
            "README.md",
            &m,
            &HashSet::new()
        )
        .is_empty());
    }

    #[test]
    fn yarn_direct_script_vs_builtin() {
        let dir = scratch("yarn");
        fs::write(
            dir.join("package.json"),
            r#"{"name":"x","scripts":{"build":"tsc"},"devDependencies":{"tsx":"4"}}"#,
        )
        .unwrap();
        let m = Manifests::load(&dir);
        // `yarn add` is a builtin (skip); `yarn build` is defined; `pnpm tsx`
        // runs a dependency's binary; `yarn lint` and `pnpm test` are undefined.
        let flagged: Vec<String> = check(
            "`yarn add foo` `yarn build` `pnpm tsx x.ts` `yarn lint` `pnpm test`",
            "README.md",
            &m,
            &HashSet::new(),
        )
        .iter()
        .filter(|f| f.verdict.is_reportable())
        .map(|f| f.detail.clone())
        .collect();
        assert_eq!(flagged.len(), 2, "{flagged:?}");
        assert!(flagged[0].contains("lint") && flagged[1].contains("test"));
    }

    #[test]
    fn project_bins_collected() {
        let dir = scratch("bins");
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"staleguard\"\n\n[[bin]]\nname = \"staleguard\"\n",
        )
        .unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        let m = Manifests::load(&dir);
        assert!(m.project_bins().contains("staleguard"));
    }
}
