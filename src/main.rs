//! shlomes command-line entry point.

mod code;
mod commands;
mod config;
mod coverage;
mod entrypoints;
mod extract;
mod findings;
#[cfg(feature = "ml")]
mod retrieve;
mod rules;
mod verify;

use code::CodeIndex;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use walkdir::WalkDir;

use crate::findings::Finding;

#[derive(Parser)]
#[command(
    name = "shlomes",
    version,
    about = "Check CLAUDE.md, project docs, and code against each other for coherence drift."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

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
        /// Max layer: 1 deterministic, 2 +retrieval, 3 +LLM (1 only for now).
        #[arg(long, default_value_t = 1)]
        layer: u8,
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
    /// Semantic code search using local jina embeddings (requires `ml` feature).
    #[cfg(feature = "ml")]
    Retrieve {
        /// Natural-language or code query.
        query: String,
        /// Repo root (default: cwd).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Number of chunks to return.
        #[arg(long, default_value_t = 5)]
        k: usize,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Text,
    Json,
}

pub(crate) fn collect_docs(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".git" && name != ".shlomes"
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("md") | Some("markdown")
            )
        })
        .collect()
}

fn run_check(root: &Path) -> Vec<Finding> {
    // Repo-wide grounding, built once and shared across every doc.
    let index = CodeIndex::build(root);
    let manifests = commands::Manifests::load(root);
    let code_tokens = config::code_tokens(root);
    let grounding = entrypoints::Grounding::from_index(&index);

    // Architecture rules: file-sourced now, prose-sourced accumulated per doc,
    // then verified once (the symbol scan walks the whole repo).
    let mut arch_rules = rules::load_file_rules(root);

    let mut findings = Vec::new();
    for doc in collect_docs(root) {
        let text = match std::fs::read_to_string(&doc) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let rel = doc
            .strip_prefix(root)
            .unwrap_or(&doc)
            .to_string_lossy()
            .to_string();
        let claims = extract::extract_path_claims(&text, &rel);
        findings.extend(verify::check_paths(&claims, root));
        findings.extend(commands::check(&text, &rel, &manifests));
        findings.extend(config::check(
            &text,
            &rel,
            &code_tokens,
            manifests.project_bins(),
        ));
        findings.extend(entrypoints::check(&text, &rel, &grounding));
        arch_rules.extend(rules::extract_prose_rules(&text, &rel));
    }
    findings.extend(rules::check(&arch_rules, &index, root));
    findings
}

fn report(findings: &[Finding], format: Format) {
    match format {
        Format::Json => {
            println!("{}", serde_json::to_string_pretty(findings).unwrap());
        }
        Format::Text => {
            if findings.is_empty() {
                println!("\u{2713} no coherence issues found");
                return;
            }
            for f in findings {
                println!("[{}] {}: {}", f.verdict.as_str(), f.doc_path, f.detail);
            }
            println!("\n{} finding(s)", findings.len());
        }
    }
}

fn report_index(index: &CodeIndex, format: Format) {
    match format {
        Format::Json => {
            println!("{}", serde_json::to_string_pretty(index).unwrap());
        }
        Format::Text => {
            for s in &index.symbols {
                println!(
                    "[{:?}/{:?}] {} ({}:{})",
                    s.kind, s.visibility, s.qualified_name, s.span.path, s.span.start_line
                );
            }
            for e in &index.edges {
                println!("edge  {} -> {}", e.from_module, e.to_module);
            }
            for e in &index.module_edges {
                println!("mod-edge  {} -> {}", e.from_module, e.to_module);
            }
            for r in &index.ref_edges {
                println!("ref-edge  {} -> {}", r.from_symbol, r.to_symbol);
            }
            println!(
                "\n{} symbol(s), {} edge(s), {} mod-edge(s), {} ref-edge(s)",
                index.symbols.len(),
                index.edges.len(),
                index.module_edges.len(),
                index.ref_edges.len()
            );
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Check {
            path,
            format,
            layer,
        } => {
            let root = std::fs::canonicalize(&path).unwrap_or(path);
            if layer > 1 {
                eprintln!("note: layers 2-3 are not implemented yet; running layer 1.");
            }
            let findings = run_check(&root);
            report(&findings, format);
            if findings.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Commands::Index { path, format } => {
            let root = std::fs::canonicalize(&path).unwrap_or(path);
            let index = CodeIndex::build(&root);
            report_index(&index, format);
            ExitCode::SUCCESS
        }
        Commands::Coverage { path, format } => {
            let root = std::fs::canonicalize(&path).unwrap_or(path);
            let findings = coverage::run(&root);
            report(&findings, format);
            if findings.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        #[cfg(feature = "ml")]
        Commands::Retrieve { query, path, k } => {
            let root = std::fs::canonicalize(&path).unwrap_or(path);
            match retrieve::retrieve(&root, std::slice::from_ref(&query), k) {
                Ok(per_query) => {
                    for hit in &per_query[0] {
                        println!("{:.3}  {}:{}", hit.score, hit.path, hit.start_line);
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}
