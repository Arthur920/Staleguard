# Staleguard details

This page covers what staleguard detects, how it works, and how it performs. For setup, CI, and editor/agent integration see the [README](README.md).

## What it detects

Every `*.md` / `*.mdx` doc in the repo is checked, along with agent rules
(`CLAUDE.md`, `AGENTS.md`, `.cursor/rules/*.mdc`, `.cursorrules`,
`.windsurfrules`, `.clinerules`), except changelogs and the
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

**New drift only.** `--diff <ref>` reports only drift introduced since a git
ref, whether a doc edit or a code change (a deleted file, a removed script)
caused it. It runs the same checks on a temporary checkout of `<ref>` and
hides findings already present there.

Everything is deterministic and tuned to under-report rather than false-alarm:
a finding always points at a concrete path, command, env var, flag, or symbol.

## Commands

```bash
staleguard check                 # full repo
staleguard check --diff main     # only drift introduced vs main
staleguard check --format json   # machine-readable findings
staleguard check --doc README.md # restrict to one doc
```

Output is `text` (human) or `json` (machine-readable). `check` exits non-zero on
any reportable finding, so it drops into CI as is.

## CI

Fail a PR only on drift it introduces. This needs the base branch fetched:

```yaml
- uses: actions/checkout@v4
  with:
    fetch-depth: 0
- uses: Arthur920/Staleguard@v0.5.0
  with:
    args: --diff origin/${{ github.base_ref }}
```

Without `--diff`, every finding in the repo fails the check.

**GitHub Action.** Inputs: `args`, `format` (`text`/`json`/`sarif`), `version`,
`working-directory`. For inline PR annotations, emit SARIF and upload it:

```yaml
- uses: Arthur920/Staleguard@v0.5.0
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
  rev: v0.5.0
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

## Agents

Stale agent instructions are worse than stale READMEs: the agent runs the
dead command and edits the moved path. To make Claude Code check its own
changes before it finishes, add a `Stop` hook to your `.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command", "command": "grep -q '\"stop_hook_active\": *true' || staleguard check --diff HEAD >&2 || exit 2" }] }
    ]
  }
}
```

`--diff HEAD` limits the check to drift the uncommitted work introduced. Exit
code 2 sends the findings back to Claude, which fixes the docs (or the code)
before it stops. The `grep` lets the next stop through (`stop_hook_active` is
set once Claude is already continuing for a Stop hook), so Claude gets one
round of feedback and can't loop if it decides the finding should stay. Other agents can run `staleguard check --diff HEAD --format
json` and use the findings (one JSON object each) in the same way.

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
