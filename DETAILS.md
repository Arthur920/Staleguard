# Staleguard details

This page covers what staleguard detects, how it works, and how it performs. For setup, CI, and editor/agent integration see the [README](README.md).

## What it detects

Every `*.md` / `*.mdx` doc in the repo is checked, except changelogs and the
content pages of a docs-site package (Astro, Docusaurus, Nextra, VitePress,
…), which describe the reader's project rather than this repo. Paths that are
absent by design are not flagged: gitignored or build output, and import paths
into the repo's own packages.

**Broken references**
- file/dir paths quoted in docs that don't exist in the repo
- commands (`npm run`, `make`, `cargo --bin`) with no matching script, target,
  or binary in `package.json` / `Makefile` / `Cargo.toml`
- env vars and CLI flags documented but never read in the code
- qualified code refs (`module::symbol`, `Type.method`) that resolve to no symbol

Where it can, a stale finding ends with a hint: ``Did you mean `…`?``, from the
file's git rename, the one tracked file with the same name, or the closest
defined script or identifier.

Paths, commands, and env vars are checked the same way in any repo. Symbol
grounding parses TypeScript/JavaScript (the tuned target) plus Rust, Python,
and Java on a best-effort basis.

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
staleguard check --doc README.md # restrict to one doc
```

Output is `text` (human) or `json` (machine-readable). `check` exits non-zero on
any reportable finding or a score regression, so it drops into CI as is.

## CI

Commit a baseline on the main branch, then fail PRs only on new drift:

```bash
staleguard check --write-ledger        # once, on main; writes .staleguard/
staleguard check --fail-on-regression  # on each PR
```

**GitHub Action.** Inputs: `args`, `format` (`text`/`json`/`sarif`), `version`,
`working-directory`. For inline PR annotations, emit SARIF and upload it:

```yaml
- uses: Arthur920/Staleguard@v0.4.0
  id: staleguard
  with:
    format: sarif
- uses: github/codeql-action/upload-sarif@v3
  if: always()
  with:
    sarif_file: ${{ steps.staleguard.outputs.sarif-file }}
```

**Pre-commit.**

```yaml
- repo: https://github.com/Arthur920/Staleguard
  rev: v0.4.0
  hooks:
    - id: staleguard
```

**Severity.** Broken references and contradictions are `error`; unconfirmable
claims are `warning`. `--min-severity error` is the strictest gate.

## Configuration

`.staleguard.toml` at the repo root, all keys optional:

```toml
exclude = ["docs/legacy/**", "NOTES.md"]  # doc globs to skip
suppress = ["unverifiable"]               # contradicted | stale | unverifiable
min_severity = "error"                    # note < warning < error; --min-severity overrides
```

Suppressed and filtered claims drop out of the alignment score's denominator, so
the score describes what you chose to check.

## Agents

Tell your agent (e.g. in `CLAUDE.md`): *"After editing code or docs, run
`staleguard check --format json` and fix any reported drift."* The JSON output
(one object per finding) also maps directly onto an MCP tool result.

## Performance and footprint

- A full `check` of a mid-size TypeScript monorepo takes **~1.2s**. Per-file
  parsing runs in parallel (rayon) and tree-sitter queries are compiled once
  and cached, so throughput scales with cores.
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
