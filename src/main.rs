//! staleguard command-line entry point.

mod check;
mod claim;
mod code;
mod commands;
mod config;
mod entrypoints;
mod extract;
mod findings;
mod git;
mod report;
mod sarif;
mod settings;
mod suggest;
mod verify;

use code::CodeIndex;
use report::Format;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use walkdir::WalkDir;

#[derive(Parser)]
#[command(
    name = "staleguard",
    version,
    about = "Check CLAUDE.md, project docs, and code against each other for coherence drift.",
    after_help = EXAMPLES
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Worked examples appended to `staleguard --help`.
const EXAMPLES: &str = "\
Examples:
  staleguard check                  full repo, deterministic (layer 1)
  staleguard check --diff main      only drift introduced since main
  staleguard check --format json    machine-readable findings (exits non-zero on drift)
  staleguard check --format sarif   SARIF for GitHub code scanning / PR annotations

Run `staleguard <command> --help` for per-command options.";

#[derive(Subcommand)]
enum Commands {
    /// Check docs against code for coherence drift.
    Check {
        /// Repo root (default: cwd).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Report only drift introduced since this git ref (e.g. `main`, or
        /// `HEAD` for uncommitted work); findings already present there are hidden.
        #[arg(long)]
        diff: Option<String>,
        /// Drop findings below this severity (`note` < `warning` < `error`) from
        /// the report, the SARIF, and the failing set. `error` keeps only broken
        /// refs and contradictions.
        /// Overrides `min_severity` in `.staleguard.toml`.
        #[arg(long, value_enum)]
        min_severity: Option<findings::Severity>,
        /// Restrict doc-vs-code checks to these doc paths (repeatable; matched by
        /// exact relative path or path suffix), e.g. to check a single changed doc.
        #[arg(long = "doc")]
        docs: Vec<String>,
    },
}

/// Every checkable doc, optionally restricted to the docs named in `filter`
/// (matched by exact relative path or path suffix, e.g. `README.md`).
/// An empty filter means "every doc" (the changelog exclusion still applies).
pub(crate) fn collect_docs_filtered(root: &Path, filter: &[String]) -> Vec<PathBuf> {
    let mut site_cache: HashMap<PathBuf, bool> = HashMap::new();
    let docs: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !crate::code::lang::is_skip_dir(&e.file_name().to_string_lossy()))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(crate::code::lang::within_size_limit)
        .map(|e| e.into_path())
        .filter(|p| {
            // `.mdc` and the dotfiles are agent rules (Cursor, Windsurf, Cline):
            // stale ones make agents run commands and edit paths that are gone.
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("md") | Some("markdown") | Some("mdx") | Some("mdc")
            ) || matches!(
                p.file_name().and_then(|s| s.to_str()),
                Some(".cursorrules") | Some(".windsurfrules") | Some(".clinerules")
            )
        })
        .filter(|p| !is_changelog_doc(p))
        .filter(|p| !in_docs_site(p, root, &mut site_cache))
        .filter(|p| {
            if filter.is_empty() {
                return true;
            }
            let rel = p.strip_prefix(root).unwrap_or(p).to_string_lossy();
            filter
                .iter()
                .any(|f| rel == f.as_str() || rel.ends_with(f.as_str()))
        })
        .collect();
    // Gitignored docs (tool scratch copies like `.stryker-tmp/`, local notes)
    // are not the repo's documentation.
    let rel = |p: &PathBuf| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    let ignored = git::ignored(root, &docs.iter().map(rel).collect::<Vec<_>>());
    docs.into_iter()
        .filter(|p| !ignored.contains(&rel(p)))
        .collect()
}

/// Dependencies that mark a package as a docs site (Astro/Starlight,
/// Docusaurus, Nextra, VitePress, VuePress, Fumadocs).
const DOCS_SITE_DEPS: &[&str] = &[
    "astro",
    "@astrojs/starlight",
    "@docusaurus/core",
    "nextra",
    "vitepress",
    "vuepress",
    "fumadocs-core",
];

/// Content pages of a docs-site package are user-facing product docs written
/// about the *reader's* project: their paths, scripts, and env vars belong to
/// the app the reader builds, not to this repo, so checking them only produces
/// false drift. The site package's own top-level README is still checked.
/// `cache` maps each package dir to whether it is a docs site.
fn in_docs_site(doc: &Path, root: &Path, cache: &mut HashMap<PathBuf, bool>) -> bool {
    let Some(doc_dir) = doc.parent() else {
        return false;
    };
    let mut dir = doc_dir;
    loop {
        let manifest = dir.join("package.json");
        if manifest.is_file() {
            let is_site = *cache.entry(dir.to_path_buf()).or_insert_with(|| {
                std::fs::read_to_string(&manifest)
                    .ok()
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .is_some_and(|v| {
                        ["dependencies", "devDependencies"].iter().any(|k| {
                            v.get(k).and_then(|d| d.as_object()).is_some_and(|d| {
                                DOCS_SITE_DEPS.iter().any(|dep| d.contains_key(*dep))
                            })
                        })
                    })
            });
            return is_site && dir != doc_dir;
        }
        if dir == root {
            return false;
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return false,
        }
    }
}

/// Changelogs and release-note fragments document *past* states, so they
/// legitimately name removed files, old symbols, and external versions: verbatim
/// history, not claims about the current code. ADRs likewise. Checking them only manufactures
/// false drift, so they are excluded from the doc set.
pub(crate) fn is_changelog_doc(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    const NAMES: &[&str] = &[
        "history.md",
        "changelog.md",
        "changelog.markdown",
        "changes.md",
        "news.md",
        "releases.md",
        "release-notes.md",
        "release_notes.md",
        "whatsnew.md",
    ];
    if NAMES.contains(&name.as_str()) {
        return true;
    }
    // Audit reports (`DOCUMENTATION_AUDIT_REPORT.md`, `audit/findings.md`) are
    // point-in-time records that list deleted files on purpose.
    let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
    if stem.contains("audit") || stem.ends_with("report") {
        return true;
    }
    // Towncrier-style fragment directories: `changes/`, `changelog.d/`, `news.d/`.
    path.components().any(|c| {
        matches!(
            c.as_os_str()
                .to_str()
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("changes")
                | Some("changelog.d")
                | Some("changelog")
                | Some("news.d")
                | Some("newsfragments")
                // Architecture decision records: dated history by design.
                | Some("adr")
                | Some("adrs")
                | Some("decisions")
                | Some("audit")
                | Some("audits")
                // Agent plan docs (`.agents/plans/`): proposals naming files
                // that don't exist yet.
                | Some("plans")
        )
    })
}

/// Every reportable finding across the repo's docs.
fn run_check(
    root: &Path,
    doc_filter: &[String],
    min_severity: Option<findings::Severity>,
) -> Vec<findings::Finding> {
    // Optional `.staleguard.toml`: doc-exclude globs + verdict suppression. A
    // malformed file aborts the run rather than silently dropping a check.
    let settings = settings::Settings::load(root).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(2);
    });
    // Repo-wide grounding, built once and shared across every doc.
    let index = CodeIndex::build(root);
    // Manifests are resolved per doc from its nearest ancestor manifest (cached
    // by directory), so a sub-project's docs check against that sub-project's
    // Makefile/Cargo.toml/package.json rather than only the repo-root one.
    let mut manifest_cache: HashMap<PathBuf, commands::Manifests> = HashMap::new();
    let code_tokens = config::code_tokens(root);
    let grounding = entrypoints::Grounding::from_index(&index);
    // The repo's path list, walked once, so each doc's path claims match in
    // memory instead of re-walking the whole tree per claim.
    let repo_files = verify::repo_paths(root);
    let pkg_names = verify::package_names(&repo_files);
    let pkg_scripts = verify::package_scripts(&repo_files);
    let ctx = check::CheckContext {
        root,
        grounding: &grounding,
        code_tokens: &code_tokens,
        repo_files: &repo_files,
        pkg_names: &pkg_names,
        pkg_scripts: &pkg_scripts,
    };
    let doc_checks = check::doc_checks();

    let mut findings = Vec::new();
    for doc_path in collect_docs_filtered(root, doc_filter) {
        let text = match std::fs::read_to_string(&doc_path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let rel = doc_path
            .strip_prefix(root)
            .unwrap_or(&doc_path)
            .to_string_lossy()
            .to_string();
        if settings.is_doc_excluded(&rel) {
            continue;
        }
        let doc_dir = doc_path.parent().unwrap_or(root).to_path_buf();
        let manifests = manifest_cache
            .entry(doc_dir.clone())
            .or_insert_with(|| commands::Manifests::load_nearest(&doc_dir, root))
            .clone();
        let doc = check::Doc {
            rel,
            text,
            manifests,
        };
        for c in doc_checks {
            findings.extend(c.check(&doc, &ctx));
        }
    }
    findings.retain(|f| f.verdict.is_reportable());
    suggest::annotate(&mut findings, root, &repo_files, &code_tokens, &pkg_scripts);

    // Verdict suppression from `.staleguard.toml` (e.g. opt out of `unverifiable`).
    settings.apply_suppression(&mut findings);
    // Severity threshold: the `--min-severity` flag wins, else the config value.
    // Same pre-pipeline placement, so dropped findings neither report nor gate.
    let threshold = min_severity.or(settings.min_severity);
    settings::Settings::apply_severity_threshold(&mut findings, threshold);
    findings
}

/// `--diff <ref>`: keep only drift introduced since `ref`. The same checks run
/// on a checkout of `ref`, and findings already there are dropped. This works for
/// every claim kind, whether the doc line or the code behind it changed.
/// Findings match on doc file + claim (not line), so edits that shift lines don't
/// resurface old drift.
fn keep_introduced(
    root: &Path,
    base: &str,
    docs: &[String],
    min_severity: Option<findings::Severity>,
    findings: &mut Vec<findings::Finding>,
) {
    let Some(wt) = git::worktree(root, base) else {
        eprintln!("warning: can't check out `{base}` (shallow clone?); reporting all findings");
        return;
    };
    let before = run_check(&wt.root, docs, min_severity);
    let key = |f: &findings::Finding| {
        let file = f.doc_path.rsplit_once(':').map_or(&*f.doc_path, |(p, _)| p);
        (file.to_string(), f.claim.clone())
    };
    let known: std::collections::HashSet<_> = before.iter().map(key).collect();
    let total = findings.len();
    findings.retain(|f| !known.contains(&key(f)));
    let hidden = total - findings.len();
    if hidden > 0 {
        eprintln!("{hidden} finding(s) already present at `{base}` not shown");
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Check {
            path,
            format,
            diff,
            min_severity,
            docs,
        } => {
            let root = std::fs::canonicalize(&path).unwrap_or(path);
            let mut findings = run_check(&root, &docs, min_severity);
            if let Some(base) = &diff {
                keep_introduced(&root, base, &docs, min_severity, &mut findings);
            }
            report::report_check(&findings, format);
            if findings.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_changelog_doc;
    use std::path::Path;

    #[test]
    fn audit_reports_are_history() {
        for p in [
            "design/DOCUMENTATION_AUDIT_REPORT.md",
            "design/audit/findings.md",
            "x/UNNECESSARY_DOCS_REPORT.md",
            ".agents/plans/calm-violet-tide.md",
        ] {
            assert!(is_changelog_doc(Path::new(p)), "{p}");
        }
        for p in ["docs/reporting.md", "docs/REPORTS_API.md", "README.md"] {
            assert!(!is_changelog_doc(Path::new(p)), "{p}");
        }
    }
}
