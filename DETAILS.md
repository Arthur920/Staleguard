# Staleguard details

This page covers what staleguard detects, how it works, and how it performs. For setup, CI, and editor/agent integration see the [README](README.md).

## What it detects

**Broken references**
- file/dir paths quoted in docs that don't exist in the repo
- commands (`npm run`, `make`, `cargo --bin`) with no matching script, target,
  or binary in `package.json` / `Makefile` / `Cargo.toml`
- env vars and CLI flags documented but never read in the code
- qualified code refs (`module::symbol`, `Type.method`) that resolve to no symbol

**Coverage gaps**
- public code surface that no doc describes, risk-ranked by fan-in, churn, and
  complexity

**Drift over time**
- `--diff <ref>` re-checks only what changed since a git ref
- a per-module and repo-wide **alignment score**, with a CI **regression gate**
- fingerprint staleness: a previously-verified claim is flagged when the code
  behind it changes

Everything is deterministic and tuned to under-report rather than false-alarm:
a finding always points at a concrete path, command, env var, flag, or symbol.
Underneath sits a **drift ledger**: it makes runs incremental, scores
alignment, and gates CI on regressions.

## Commands

```bash
staleguard check                 # full repo
staleguard check --diff main     # only what changed vs main
staleguard check --format json   # machine-readable findings
staleguard check --doc README.md # restrict to one doc (cheaper)

staleguard index                 # code symbols + module/reference edges (tree-sitter)
staleguard coverage              # public code surface that no doc describes
```

Output is `text` (human) or `json` (machine-readable). `check` exits non-zero on
any reportable finding or a score regression, so it drops into CI as is.

## Performance and footprint

- Staleguard scans a ~330k-line repo (1,363 source
  files) in **~1.2s** for a full `check` (~0.7s warm) and **under a second**
  for `index`, at ~100 MB peak memory. Per-file parsing runs in parallel
  (rayon) and tree-sitter queries are compiled once and cached, so throughput
  scales with cores.
- Nothing leaves the machine: no models, no network access.

## About this project

Staleguard is a personal project, and its development was **heavily AI-assisted**.
Most of the implementation was written with AI coding tools. I directed
the architecture, the layer design, and the evaluation, and decided what was good
enough to keep. An experimental ML layer (local embeddings plus a fine-tuned
NLI judge) was built and evaluated in the same loop, then removed: its recall on
subtle contradictions was too low to justify the weight (see git history). The ideas and the judgment calls are mine, but a large share of the code is not
hand-typed. That is why the deterministic core is built to be auditable: I
don't expect anyone (including me) to take generated code on faith.
