//! Output rendering for every `staleguard` subcommand.
//!
//! One place that knows how to turn each command's result (findings, a drift
//! [`Outcome`](crate::drift::Outcome), the rule audit, the code index) into
//! either human `text` or machine `json`, so the command dispatch in `main.rs`
//! stays argument-parsing plus a render call.

use crate::drift;
use crate::findings::{Finding, Verdict};

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

/// Findings grouped under their doc, sorted by line, one per row:
///
/// ```text
/// README.md
///   31  script `demo:setup` is not defined
///   97  script `demo:reset` is not defined; did you mean `db:reset`?
/// ```
fn render_text(findings: &[Finding]) -> String {
    use std::fmt::Write;
    if findings.is_empty() {
        return "\u{2713} no stale docs found\n".into();
    }
    let split = |f: &Finding| match f.doc_path.rsplit_once(':') {
        Some((doc, line)) if line.parse::<usize>().is_ok() => {
            (doc.to_string(), line.parse().unwrap())
        }
        _ => (f.doc_path.clone(), 0),
    };
    let mut rows: Vec<(String, usize, &Finding)> = findings
        .iter()
        .map(|f| {
            let (doc, line) = split(f);
            (doc, line, f)
        })
        .collect();
    rows.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    let width = rows
        .iter()
        .map(|r| r.1.to_string().len())
        .max()
        .unwrap_or(1);

    let mut out = String::new();
    let mut docs = 0;
    let mut current = "";
    for (doc, line, f) in &rows {
        if doc != current {
            if docs > 0 {
                out.push('\n');
            }
            docs += 1;
            current = doc;
            let _ = writeln!(out, "{doc}");
        }
        let tag = match f.verdict {
            Verdict::Stale => String::new(),
            v => format!("[{}] ", v.as_str()),
        };
        let _ = writeln!(out, "  {line:>width$}  {tag}{}", f.detail);
    }
    let _ = writeln!(out, "\n{} finding(s) in {docs} doc(s)", rows.len());
    out
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
            print!("{}", render_text(&out.findings));
            println!(
                "alignment {:.3} | {} claim(s), {} carried forward",
                out.score.repo, out.total_claims, out.carried_forward
            );
            if let Some((base, head)) = out.regression {
                println!("\u{2717} score regressed: {base:.3} (base) -> {head:.3} (head)");
            }
        }
    }
}
