<p align="center">
  <img src="shlomes.png" alt="shlomes logo" width="320">
</p>

# shlomes

A CLI that sanity-checks your `CLAUDE.md`, project docs (`*.md`), and the actual
codebase against each other to surface **coherence drift** — places where the
docs claim something the code no longer (or never did) backs up.

> Status: all three layers implemented. Layer 1 (deterministic) is the default
> build; Layers 2–3 (local retrieval + NLI judge) live behind the `ml` feature,
> fully offline after a one-time model download.

## Architecture: a 3-layer hybrid

```
docs (.md, CLAUDE.md)                         codebase
        │                                         │
        ▼                                         ▼
 ┌──────────────┐                        ┌────────────────┐
 │  extract     │  atomic claims         │  index         │  files, AST facts,
 │  claims      │ ───────────────┐       │  (tree-sitter, │  commands, config keys
 └──────────────┘                │       │   ctags, glob) │
        │                        │       └────────────────┘
        ▼                        │                │
 ┌──────────────────────────────┴────────────────┴───────┐
 │ Layer 1 — DETERMINISTIC  (no ML, zero false positives) │
 │   file paths exist? commands real? env vars/config     │
 │   keys present? entry points valid?                    │
 ├────────────────────────────────────────────────────────┤
 │ Layer 2 — RETRIEVAL  (embeddings + optional reranker)  │
 │   for each surviving claim, fetch most-relevant code   │
 ├────────────────────────────────────────────────────────┤
 │ Layer 3 — VERIFICATION  (LLM-as-judge / NLI)           │
 │   claim + evidence → supported | contradicted | stale  │
 └────────────────────────────────────────────────────────┘
        │
        ▼
   findings report (text / json / sarif)
```

### Layer 1 — Deterministic checks
The cheapest, highest-signal layer. Many doc claims are concrete and verifiable
without any model:
- file/dir paths quoted in docs that don't exist
- commands (`npm run`, `make`, `cargo` with `--bin`) with no matching script,
  target, or binary in the package.json / Makefile / Cargo.toml manifest
- env vars and CLI flags referenced in docs but never read in the code
- qualified code references (module::symbol, Type::method) that resolve to no
  symbol or module in the tree-sitter index
- architecture rules stated in doc prose (forbidden imports, layering,
  forbidden symbols) that the dependency graph violates → `contradicted`

Runs in milliseconds, no API cost, no false positives. Each check only fires
against grounding that actually exists (no manifest ⇒ no command findings; an
external `std::…` path is never our claim), so it under-reports rather than ever
flagging a false positive. Catches a large share of real drift on its own.

### Layer 2 — Retrieval (this is where embeddings belong)
For claims that aren't deterministically checkable ("the cache invalidates on
write"), embed doc claims and code chunks, retrieve the top-k relevant code via
cosine similarity, optionally rerank.

Implemented with **local embeddings** via [`fastembed`](https://crates.io/crates/fastembed)
and the `jina-embeddings-v2-base-code` model (ONNX, ~160 MB, downloaded once then
fully offline). Code never leaves the machine. Chunking is **symbol-aligned**
from the tree-sitter index (line-window fallback for symbol-less files), and a
**content-hash vector cache** under `.shlomes/` makes unchanged chunks free on
re-run — the embedding model only loads when something actually needs embedding.
An **optional cross-encoder reranker** (`SHLOMES_RERANK_REPO`) sharpens the
top-k before it reaches the judge.

### Layer 3 — Verification (NLI cross-encoder judge)
The actual coherence judgment: an NLI cross-encoder reads
`(evidence, claim)` and classifies `entailment / contradiction / neutral` →
`supported / contradicted / unverifiable`. A classifier, **not** a generative
LLM — no API, no per-token cost, code never leaves the machine (default
`nli-deberta-v3-xsmall`, int8 ONNX ~20 MB; overridable via `SHLOMES_NLI_*`).
This is the contradiction axis embeddings *cannot* do alone. Claims ground their
backtick tokens to symbols, so a `supported` verdict is ledgered and re-opens
(`stale`) via the Layer 0 fingerprint flag when the anchored code changes.

## Performance / cost

- **Content-hash cache** for embeddings and claim extraction — unchanged files
  are free on re-run.
- **`--diff` mode**: scope a run to files changed vs a git ref, so CI checks only
  touch what moved.

## Usage

```bash
shlomes check                 # full repo (layer 1, deterministic)
shlomes check --diff main     # only what changed vs main
shlomes check --format json   # machine-readable findings
shlomes check --layer 1       # deterministic only (no model needed)
shlomes check --layer 3       # + retrieval + NLI judge (requires the `ml` build)

shlomes index                 # code symbols + module edges + symbol reference edges (tree-sitter)
shlomes index --format json   # machine-readable index

shlomes coverage              # public code surface that no doc describes (code -> doc gaps)
shlomes coverage --format json

# Layer 2 — local semantic code search (requires the `ml` feature build)
shlomes retrieve "where is auth handled" --k 5
```

## Build (dev)

```bash
cargo build                          # debug binary at target/debug/shlomes
cargo test                           # unit tests (layer 1)
cargo run -- check .                 # run against this repo

# Layers 2-3 (local embeddings + NLI judge) are behind the `ml` feature:
cargo build --features ml
cargo run --features ml -- retrieve "query" --k 5
cargo run --features ml -- check --layer 3

cargo install --path . --features ml # install the `shlomes` binary (with ml)
```

Layer 1 (deterministic) builds with no extra features. Layers 2 (retrieval) and 3
(NLI judge) live behind the `ml` feature so the default build stays lean.
