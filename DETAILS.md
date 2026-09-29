# Staleguard details

This page covers what staleguard detects, how the layers work, and how it performs. For setup, CI, and editor/agent integration see the [README](README.md).

## What it detects

**Broken references**
- file/dir paths quoted in docs that don't exist in the repo
- commands (`npm run`, `make`, `cargo --bin`) with no matching script, target,
  or binary in `package.json` / `Makefile` / `Cargo.toml`
- env vars and CLI flags documented but never read in the code
- qualified code refs (`module::symbol`, `Type.method`) that resolve to no symbol

**Architecture violations**: rules parsed straight from prose and checked against
the real import graph.
- forbidden imports: "`controllers` must not import `db`" (direct edge). Add
  "transitively", "indirectly", or "reach" ("`controllers` must not transitively
  import `db`") to forbid *any* import chain, not only a direct one. The
  violation names the offending path.
- layering: "`domain` depends on nothing", "`api` may only depend on `domain`"
- independence: "`core` is independent of `infra`"
- forbidden symbols: "no direct `os.environ` outside `config`" (text scan plus
  resolved references)

**Behavioral contradictions** *(experimental, `ml` feature)*: a local NLI
cross-encoder (no LLM API) judges prose the deterministic layer can't, e.g. "the
cache invalidates on write".
- verdicts `supported` / `contradicted` / `unverifiable`, each with a confidence
- claims ground to symbols, so a verdict re-opens when that code changes
- advisory only: high precision, uneven recall (see [Status](#status))

**Coverage gaps**
- public code surface that no doc describes, risk-ranked by fan-in, churn, and
  complexity

**Diagram coherence**
- Mermaid / PlantUML / Graphviz diagrams diffed against the real dependency
  graph to find phantom edges, stale boxes, and missing arrows

**Drift over time**
- `--diff <ref>` re-checks only what changed since a git ref
- a per-module and repo-wide **alignment score**, with a CI **regression gate**
- fingerprint staleness: a previously-verified claim is flagged when the code
  behind it changes

**Layer 1 is the product.** It's deterministic, ships in every build, and is
tuned to under-report rather than false-alarm. Layers 2 and 3 are an **experimental,
opt-in ML extension** (the `ml` feature) that tries to catch behavioral drift the
deterministic core can't. They are useful but advisory; the evaluation is below.

```
 Layer 1 — DETERMINISTIC  (no ML, zero false positives)         ← default, the tool
   paths exist? commands real? config keys present? entry points valid?
   architecture rules from prose vs the import graph → contradicted
 Layer 2 — RETRIEVAL  (local embeddings + optional reranker)    ← experimental
   for each surviving claim, fetch the most-relevant code chunks
 Layer 3 — VERIFICATION  (local NLI cross-encoder)              ← experimental
   (evidence, claim) → supported | contradicted | unverifiable
```

Each layer is cheaper and higher-signal than the next, so most
drift is caught before any model runs. Underneath sits a **drift ledger**
(Layer 0): it makes runs incremental, scores alignment, and gates CI on
regressions.

## Commands

```bash
staleguard check                 # full repo (layer 1, deterministic)
staleguard check --diff main     # only what changed vs main
staleguard check --format json   # machine-readable findings
staleguard check --layer 3       # + retrieval + NLI judge (requires the `ml` build)
staleguard check --doc README.md # restrict to one doc (cheaper; scopes layer 3)

staleguard index                 # code symbols + module/reference edges (tree-sitter)
staleguard coverage              # public code surface that no doc describes
staleguard retrieve "where is auth handled" --k 5   # local semantic code search (ml)
```

Output is `text` (human) or `json` (machine-readable). `check` exits non-zero on
any reportable finding or a score regression, so it drops into CI as is.

## Performance and footprint

- The deterministic Layer 1 scans a ~330k-line repo (1,363 source
  files) in **~1.2s** for a full `check` (~0.7s warm) and **under a second**
  for `index`, at ~100 MB peak memory. Per-file parsing runs in parallel
  (rayon) and tree-sitter queries are compiled once and cached, so throughput
  scales with cores.
- Models run locally and offline. The jina embedding model (~160 MB) and the code-aware
  NLI cross-encoder (`staleguard`, a UniXcoder fine-tune, int8
  ONNX ~121 MB) download once from the Hub, then run on-device. Code never leaves
  the machine.
- Content-hash caches make unchanged files and code chunks free on re-run.
  The embedding model loads only when something needs embedding.
- Code is chunked on tree-sitter symbol boundaries
  (line-window fallback), with an optional reranker (`STALEGUARD_RERANK_REPO`).
- The default build is lean: Layer 1 pulls no ML dependencies. Embeddings and the
  judge live behind the `ml` feature.

## Layer 3 cost

Layer 3 is the heaviest pass: one cross-encoder forward per (claim × evidence)
chunk, ~0.14s/claim on CPU. It caps at `STALEGUARD_NLI_MAX_CLAIMS` claims per run
(default 300; `0` = no cap), which is the main time/coverage knob. On a large repo a
capped run is ~45s; uncapped scales linearly with claim count.

## Environment overrides

| Variable | Effect |
|---|---|
| `STALEGUARD_NLI_REPO` | NLI judge model repo (default `Arthur920/staleguard`) |
| `STALEGUARD_NLI_ONNX` | ONNX artifact within the repo |
| `STALEGUARD_NLI_THRESHOLD` / `STALEGUARD_NLI_MARGIN` | decision thresholds |
| `STALEGUARD_NLI_MAX_CLAIMS` | per-run claim budget (default 300; `0` = no cap) |
| `STALEGUARD_EMBED_REPO` / `STALEGUARD_EMBED_ONNX` | Layer 2 embedding model |
| `STALEGUARD_RERANK_REPO` | optional reranker |
| `STALEGUARD_ORT_THREADS` | ONNX intra-op threads (default: all cores) |

## Status

- **Layer 1** is the product and what most runs should rely on. It is deterministic,
  shipped by default, and tuned to under-report rather than false-alarm. The `ml`
  layers below are opt-in and advisory.
- **Layer 2**, the retrieval feeder that decides what code each claim is judged
  against, is the solid half of the ml build. A recall harness
  (`layer2_recall_*` in `src/evidence.rs`) measures it. On a labelled
  fixture corpus the model-free default (grounding + lexical fallback) reaches
  **recall@5 0.90**. Its one miss is a deliberately low-overlap paraphrase, and
  the optional embedding retriever (`STALEGUARD_EMBED_RETRIEVE`) recovers that
  miss for **recall@5 1.00**. The model-free harness runs in normal CI as a
  regression gate; the embedding one is `#[ignore]`d (loads the ~160 MB model).
- **Layer 3** (the NLI judge) is newer. The default model,
  [`staleguard`](https://huggingface.co/Arthur920/staleguard),
  is a `microsoft/unixcoder-base` fine-tune trained for this task. It is
  code-aware, so real code stays in-distribution as the premise. An
  earlier text-NLI model was not, and produced overconfident false
  contradictions. Treat its verdicts as advisory and review contradictions
  before acting. Two in-tree harnesses measure its ability. Both are
  `#[ignore]`d because they load the 121 MB model; run them with
  `cargo test --features ml holdout -- --ignored` and `... e2e ...`.
  - The **ability benchmark** is a class-balanced slice of the repo-disjoint holdout
    split the model was trained against (generated locally by
    `tools/gen_holdout_sample.py`; not vendored, since the snippets are
    third-party OSS). On it the model scores **contradiction precision 0.89,
    recall 0.92, 3-class accuracy 0.83** on code from unseen repos: when it flags
    drift it is almost always real drift.
  - The **adversarial probe** (`nli_e2e_corpus.jsonl`) has hard *minimal-pair* negations
    and constant swaps ("defaults to 8080" vs `unwrap_or(5432)`). Here recall is
    low: the cross-encoder leans on lexical overlap and reads many subtle
    negations as supported, so Layer 3 under-reports the *closest* paraphrase-level
    drift.

  A Contradicted verdict is trustworthy, but silence is not proof of coherence,
  especially for one-token logic flips. Both harnesses double as regression baselines for
  retraining the model.

## About this project

Staleguard is a personal project, and its development was **heavily AI-assisted**.
Most of the implementation was written with AI coding tools. I directed
the architecture, the layer design, and the evaluation, and decided what was good
enough to keep. The custom Layer 3 model was trained and evaluated in the same
loop. The ideas and the judgment calls are mine, but a large share of the code is not
hand-typed. That is why the deterministic core is built to be auditable: I
don't expect anyone (including me) to take generated code on faith.
