//! Output rendering for every `staleguard` subcommand.
//!
//! One place that knows how to turn each command's result (findings, a drift
//! [`Outcome`](crate::drift::Outcome), the rule audit, the code index) into
//! either human `text` or machine `json`, so the command dispatch in `main.rs`
//! stays argument-parsing plus a render call.

use crate::drift;
use crate::findings::Finding;

use clap::ValueEnum;

/// How a command renders its result: `text` (human), `json` (machine-readable),
/// or `sarif` (GitHub code-scanning; `check` only).
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Text,
    Json,
    /// SARIF 2.1.0, for GitHub code scanning.
    Sarif,
}

pub(crate) fn report(findings: &[Finding], format: Format) {
    match format {
        Format::Sarif => {
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::sarif::render(findings)).unwrap()
            );
        }
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

/// Report a completed drift run: the findings plus the alignment score and the
/// lineage/regression summary.
pub(crate) fn report_check(out: &drift::Outcome, format: Format) {
    match format {
        Format::Sarif => {
            println!(
                "{}",
                serde_json::to_string_pretty(&crate::sarif::render(&out.findings)).unwrap()
            );
        }
        Format::Json => {
            let payload = serde_json::json!({
                "findings": out.findings,
                "score": out.score,
                "carried_forward": out.carried_forward,
                "total_claims": out.total_claims,
                "regression": out.regression.map(|(b, h)| serde_json::json!({ "base": b, "head": h })),
            });
            println!("{}", serde_json::to_string_pretty(&payload).unwrap());
        }
        Format::Text => {
            report(&out.findings, format);
            println!(
                "\nalignment {:.3} | {} claim(s), {} carried forward",
                out.score.repo, out.total_claims, out.carried_forward
            );
            if let Some((base, head)) = out.regression {
                println!("\u{2717} score regressed: {base:.3} (base) -> {head:.3} (head)");
            }
        }
    }
}
