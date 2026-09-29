//! staleguard command-line entry point.

mod check;
mod claim;
mod code;
mod commands;
mod config;
mod coverage;
mod drift;
mod entrypoints;
mod extract;
mod findings;
mod git;
mod report;
mod sarif;
mod settings;
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
  staleguard check --diff main      only re-check what changed vs main
  staleguard check --format json    machine-readable findings (exits non-zero on drift)
  staleguard check --format sarif   SARIF for GitHub code scanning / PR annotations
  staleguard check --write-ledger   set the CI alignment baseline on the base branch
  staleguard index                  print code symbols + module/reference edges
  staleguard coverage               public code surface no doc describes

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
        /// Drift base: only re-derive claims whose code changed since this git
        /// ref (default: the committed ledger's last commit).
        #[arg(long)]
        diff: Option<String>,
        /// Persist the drift ledger + alignment score under `.staleguard/` (run this
        /// on the base branch to set the CI baseline).
        #[arg(long)]
        write_ledger: bool,
        /// Fail if the alignment score regressed below the committed baseline.
        #[arg(long)]
        fail_on_regression: bool,
        /// Drop findings below this severity (`note` < `warning` < `error`) from
        /// the report, the SARIF, and the failing set. Default `warning` hides the
        /// high-volume `undocumented` notes and shows only provable drift; `note`
        /// keeps everything (including the undocumented-surface coverage report);
        /// `error` keeps only broken refs and contradictions.
        /// Overrides `min_severity` in `.staleguard.toml`.
        #[arg(long, value_enum)]
        min_severity: Option<findings::Severity>,
        /// Restrict doc-vs-code checks to these doc paths (repeatable; matched by
        /// exact relative path or path suffix). Skips the repo-wide coverage and
        /// history passes, so it is far cheaper, useful for checking a single
        /// changed doc.
        #[arg(long = "doc")]
        docs: Vec<String>,
    },
    /// Extract and print the code index (symbols + dependency edges).
    Index {
        /// Repo root (default: cwd).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Report public code surface that no doc describes (code -> doc gaps).
    Coverage {
        /// Repo root (default: cwd).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Output format.
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
}

pub(crate) fn collect_docs(root: &Path) -> Vec<PathBuf> {
    collect_docs_filtered(root, &[])
}

/// `collect_docs`, optionally restricted to the docs named in `filter` (matched
/// by exact relative path or path suffix, e.g. `README.md` or `docs/usage.md`).
/// An empty filter means "every doc" (the changelog exclusion still applies).
pub(crate) fn collect_docs_filtered(root: &Path, filter: &[String]) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !crate::code::lang::is_skip_dir(&e.file_name().to_string_lossy()))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(crate::code::lang::within_size_limit)
        .map(|e| e.into_path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("md") | Some("markdown")
            )
        })
        .filter(|p| !is_changelog_doc(p))
        .filter(|p| {
            if filter.is_empty() {
                return true;
            }
            let rel = p.strip_prefix(root).unwrap_or(p).to_string_lossy();
            filter
                .iter()
                .any(|f| rel == f.as_str() || rel.ends_with(f.as_str()))
        })
        .collect()
}

/// Changelogs and release-note fragments document *past* states, so they
/// legitimately name removed files, old symbols, and external versions: verbatim
/// history, not claims about the current code. Checking them only manufactures
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
        )
    })
}

fn run_check(
    root: &Path,
    opts: &drift::Options,
    doc_filter: &[String],
    min_severity: Option<findings::Severity>,
) -> drift::Outcome {
    // `--doc` scoping: restrict every doc-derived pass to the named docs and skip
    // the repo-wide coverage/history passes (which answer "what code is
    // undocumented", a whole-repo question that a single-doc check doesn't ask).
    // This is what makes a scoped run cheap: no 1000-commit history parse and no
    // coverage ranking.
    let scoped = !doc_filter.is_empty();
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
    // One git-history fetch shared by every history-mining pass (coverage risk
    // ranking). Skipped entirely when scoped.
    let history = if scoped {
        Vec::new()
    } else {
        git::file_change_history(root, drift::coupling::MAX_COMMITS)
    };
    // The repo's path list, walked once, so each doc's path claims match in
    // memory instead of re-walking the whole tree per claim.
    let repo_files = verify::repo_paths(root);
    let ctx = check::CheckContext {
        root,
        grounding: &grounding,
        code_tokens: &code_tokens,
        repo_files: &repo_files,
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

    // Code -> doc coverage gaps: undocumented public surface, anchored to its
    // symbol so it scores as its own dimension of the alignment score. This is a
    // whole-repo question, so a `--doc`-scoped run skips it.
    if !scoped {
        findings.extend(coverage::gaps(&index, root, &history));
    }

    // Verdict suppression from `.staleguard.toml` (e.g. opt out of `undocumented`).
    // Applied before the drift pipeline so suppressed findings neither report nor
    // gate. `Supported` claims are untouched, so the alignment score is unaffected.
    settings.apply_suppression(&mut findings);
    // Severity threshold: the `--min-severity` flag wins, else the config value,
    // else the default. The default is `warning`, which hides the high-volume
    // `undocumented` notes so the out-of-the-box run shows only provable drift
    // (broken refs, contradictions). Pass `--min-severity note` to see everything,
    // including the undocumented-surface coverage report.
    // Same pre-pipeline placement, so dropped findings neither report nor gate.
    let threshold = min_severity
        .or(settings.min_severity)
        .or(Some(findings::Severity::Warning));
    settings::Settings::apply_severity_threshold(&mut findings, threshold);
    drift::run(findings, &index, root, opts)
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Check {
            path,
            format,
            diff,
            write_ledger,
            fail_on_regression,
            min_severity,
            docs,
        } => {
            let root = std::fs::canonicalize(&path).unwrap_or(path);
            let opts = drift::Options {
                diff_ref: diff,
                write_ledger,
                fail_on_regression,
            };
            let out = run_check(&root, &opts, &docs, min_severity);
            report::report_check(&out, format);
            // Fail on any reportable finding, or on a score regression in CI.
            if out.findings.is_empty() && out.regression.is_none() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Commands::Index { path, format } => {
            let root = std::fs::canonicalize(&path).unwrap_or(path);
            let index = CodeIndex::build(&root);
            report::report_index(&index, format);
            ExitCode::SUCCESS
        }
        Commands::Coverage { path, format } => {
            let root = std::fs::canonicalize(&path).unwrap_or(path);
            let findings = coverage::run(&root);
            report::report(&findings, format);
            if findings.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}
