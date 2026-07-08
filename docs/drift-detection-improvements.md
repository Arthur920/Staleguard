# Improving Layer-1 Drift Detection — Roadmap Notes

Concrete improvement points for the deterministic detectors, grounded in the
current code (`src/rules/`, `src/diagram/`, `src/constswap.rs`, and the
reference/config passes). Ordered roughly by expected recall gain per unit of
FP risk. The invariant everything below must preserve: **Layer 1 stays
zero-false-positive; recall grows only through grounding, never guessing.**

---

## 1. Architectural prose rules (`src/rules/`)

### 1.1 Promote the bare-operand extractor out of audit-only
`extract_bare_rules` is wired into `staleguard rules` only. The eval memory
says real prose is too messy for single-token grounding, but the path-style
operand branch of `canonical_operand` (segment/subtree matching) is much
tighter than the single-word branch. Split the promotion:
- promote **path-style bare operands** (`dafny/specs`) into `check` first —
  they carry a separator, so accidental English is near-impossible;
- keep single-word bare operands audit-only until the prose eval corpus shows
  precision ≥ the backticked path.

### 1.2 Multi-operand lists on one line ✅ (implemented)
`extract_prose_rules` captures exactly one operand per side. Common real
phrasings are lists: "`api` must not import `db` or `cache`",
"`ui`, `cli` must not depend on `internal`". Extend the edge regexes to
capture a backtick-token *list* on either side (reuse `backtick_tokens`) and
fan out to N rules. Pure recall, zero precision risk — every operand is still
backticked and grounded.

### 1.3 Directional "allowed" phrasings (positive rules)
Only `Layer` ("only depends on", "depends on nothing") captures allow-lists.
Missing common positive forms whose *violation* is checkable:
- "dependencies flow inward: `api` → `domain` → `core`" (an ordered layering
  chain — compile to pairwise `Layer` rules);
- "`X` is only used by `Y`" (reverse allow-list: forbid every other inbound
  edge to `X`);
- "all database access goes through `repository`" (a ForbidSymbol/edge hybrid:
  forbid db-module edges except from `repository`).

### 1.4 Sentence-level extraction, not line-level ✅ (line-joining + sentence split implemented — see 7.3)
Both extractors iterate `markdown.lines()`. A rule wrapped across a hard line
break ("`controllers` must not\nimport `db`") is silently missed — the exact
under-report the prose-eval harness is built to catch. Join soft-wrapped
lines within a paragraph (blank-line delimited) before matching; keep the
origin as the paragraph's first line.

### 1.5 Rule provenance in bullets/tables
Architecture docs often state rules in tables (`| controllers | must not
import | db |`) and bullet fragments ("- no `db` imports from `controllers`").
The current regexes need a full sentence shape. Add a table-row extractor
(cells are already delimited — precision is easy) and the fragment phrasing.

### 1.6 `ForbidReach` for independence and layer rules ✅ (independence → symmetric ForbidReach implemented)
"`domain` is independent of `infra`" compiles to two direct `ForbidEdge`s, but
independence prose almost always means *no path*. A transitive chain
`domain → shared → infra` today passes silently. Either compile independence
to symmetric `ForbidReach`, or report a `note:`-level finding when the direct
rule holds but a path exists (advisory, still zero hard-FP).

### 1.7 Stopword denylist → shape heuristics
`BARE_STOPWORDS` is a hand-grown 70-entry list and will keep leaking (already
has a duplicated `concretions`/`abstractions` pair). Complement it with shape
rules: reject operands that are dictionary-common English *and* ground only
via a case-insensitive single-segment match; require exact-case match for
single-word bare operands. Fewer list entries, same safety.

### 1.8 Negated-context guards ✅ (hedge pre-filter implemented; see 7.3 for the remaining mid-sentence gap)
`quoted_re` strips quoted examples and fences are skipped, but conditional or
historical prose still matches: "previously, `api` could not import `db`",
"if `api` must not import `db`, use the port". Add a cheap pre-filter that
skips lines starting with/containing hedges (`previously`, `used to`, `if`,
`unless`, `for example`, `e.g.`) before the rule regexes run. Audit-mode can
report what the hedge filter dropped, so recall loss is measurable.

## 2. Diagram coherence (`src/diagram/`)

### 2.1 Sequence/class/state/ER depth
The graph-shaped diff is the mature path; the symbol-grounded kinds are
shallow:
- **sequence** (`align.rs`): messages align to the call graph, but only
  participant-to-participant. Ground the *message label* itself
  (`A -> B : createUser()`) against `B`'s symbol set — a named method that no
  longer exists is a precise Stale finding.
- **class**: fields are skipped as ambiguous (`class.rs:130`). Fields with a
  declared type (`+name: String`) are groundable against `Facts` — check the
  member exists, even if the type stays advisory.
- **state**: ground transition labels (event/guard names) against symbols, not
  just state names.

### 2.2 Subgraph / package containment
Mermaid `subgraph` and PlantUML `package` blocks assert *membership*
("`auth` lives inside `services`"). The parsers flatten these. Containment is
directly checkable against module paths: a box drawn inside `services` whose
real path is `src/billing/auth` is drift no edge-diff can see.

### 2.3 Edge labels as rule carriers ✅ (deprecated/TODO suppression implemented in all three parsers)
A labeled edge `api -->|reads| db` asserts more than an edge. First cut:
treat "uses/calls/imports"-family labels as confirmation, but treat an edge
labeled `deprecated` or `TODO` as intentionally non-real (suppress the
phantom-edge finding) — that's an FP class waiting to happen.

### 2.4 Fuzzy-resolution upgrade path for phantom edges
Phantom-edge and missing-arrow checks require `exact_module()` on both ends —
correct post-wild-audit, but it silences whole diagrams whose boxes ground
fuzzily. Add an advisory (`note`-level, non-gating) tier for
fuzzy-both-ends phantom edges, surfaced only in the audit report, so the
recall ceiling becomes measurable before deciding to promote.

### 2.5 Direction on undirected edges
`A --- B` is accepted if *either* direction exists. Many authors use `---`
sloppily where they mean dependency. Leave the check as-is, but when a
directed edge `A --> B` exists only as `B -> A` in the graph, report a
*reversed arrow* finding — currently that misdraw is reported as phantom
(confusing) or missed entirely if the reverse edge also fires the exists
check.

### 2.6 More formats
D2 and Structurizr DSL are growing in exactly the docs-as-code repos that are
staleguard's audience; both are line-oriented and easier to parse than
PlantUML. ASCII-art box diagrams are common in READMEs (staleguard's own
DETAILS.md layer diagram!) — likely audit-only forever, but worth measuring.

## 3. Constant-swap (`src/constswap.rs`)

### 3.1 Value types beyond int/bool/plain-string
Floats are canonicalized in `Value::Int` handling but durations ("defaults to
`30s`", "5 minutes") and sizes ("1 MB", `1024`) are not: normalize a small
unit table (s/ms/m/h, KB/MB) on both sides before comparing.

### 3.2 Enum-like membership claims
"`log_level` defaults to `info`" where the code holds `"warn"` is caught, but
"`mode` is one of `a`, `b`, `c`" (a set claim vs. a code-side enum/match) is
not. The set is checkable against `Facts` variants with the same
one-symbol-uniqueness gate.

### 3.3 Cross-file default resolution
Rule 2 requires exactly one symbol name match repo-wide; a config key defined
in both a dataclass and a JSON-schema default silently disqualifies itself.
When multiple matches carry the *same* literal, the claim is still decidable —
compare against the agreed value instead of going silent.

### 3.4 Table-format claims ✅ (implemented)
Config docs overwhelmingly state defaults in tables
(`| PORT | 8080 | ... |`), which don't match the sentence-shaped `claim_re`.
A markdown-table extractor with a header sniff (`default` column) would likely
double recall on real repos; operands are cell-delimited, so precision holds.

## 4. Reference / command / config passes

- **Anchor-aware path checks**: `docs/foo.md#section` — verify the heading
  exists, not just the file. Cheap, deterministic, common drift.
- **Version-pinned commands**: `npm run build` grounds against
  `package.json`, but flags (`--workspace=x`) and script *arguments* are not
  checked against the script body. Audit-only first.
- **Env var write-side**: vars documented and read are verified; a var the
  code reads but no doc mentions is only visible via `coverage`. Fold
  undocumented-but-load-bearing env vars into the coverage risk ranking
  (fan-in already exists as a signal).

## 5. Cross-cutting

### 5.1 One shared audit funnel
Bare rules, fuzzy diagram edges, table claims — every proposal above follows
the same promote-via-audit lifecycle, but each detector wires its own
audit-only flag ad hoc. Make "advisory tier" a first-class `Finding`
property (gating vs. non-gating) so `rules`, diagram, and constswap
experiments all report through one channel and the wild-audit tooling
(41-repo constswap, 10-repo diagram runs) works on all of them unchanged.

### 5.2 Grounding index reuse
`canonical_operand`, `diagram::ground::resolve`, and constswap's symbol match
are three separate grounding implementations with three different
case/segment policies. Unify on one module/symbol resolution API with an
explicit `Exact | Fuzzy | Ambiguous | None` result (diagram's `Resolution` is
the best of the three) — every consistency bug found in one detector's wild
audit currently has to be re-fixed in the others.

## 6. Further points (second pass)

### 6.1 Rule scope exceptions — tests and generated code
Real architecture rules are almost always implicitly scoped: "`domain` must
not import `infra`" never means test fixtures. Today a `tests/` import edge
violates the rule the same as production code. Support explicit scope
phrasings ("except in tests", "outside `tests/`") on *edge* rules the way
`except_modules` already does for `ForbidSymbol`, and consider a default
carve-out for test/generated directories (behind a config flag, since some
teams do want tests constrained).

### 6.2 Import-graph fidelity is the recall ceiling
Every rule/diagram check is only as good as `module_edges`. Known blind
spots worth auditing per language: re-exports (a facade module laundering a
forbidden dep), dynamic imports (`importlib`, `require(expr)`, reflection),
and dependency injection (the edge lives in a wiring/config file, not an
import). A wild audit that counts *missed real edges* per language would show
where rule verification silently passes on violated rules — the worst failure
mode, since a "holds" claim is emitted.

### 6.3 Cross-doc rule contradictions
Two docs stating incompatible rules ("`api` may only depend on `domain`" vs.
an ADR saying "`api` calls `cache` directly") are both checked against code,
but never against each other. All rules already compile into one `Rule`
vocabulary — a cheap pairwise pass can flag doc-vs-doc conflicts (a `Layer`
allow-list that excludes an edge another doc asserts as intended).

### 6.4 Prose arrow chains
Docs state dependency direction with literal arrows outside any diagram:
"data flows `api` → `domain` → `core`" or `cli -> core -> store` in a
paragraph. These are backtick-anchored and groundable — compile each arrow
pair to a checkable directed-edge assertion (exists → supported; missing →
advisory, since prose arrows are sometimes dataflow, not imports).

### 6.5 Countable prose claims
"The crate exposes three subcommands", "there are 5 supported formats" —
numeric claims about enumerable code facts (subcommands, variants, public
modules) drift constantly and are deterministically checkable when the
enumeration grounds (e.g. clap subcommands, enum variants). Narrow phrasing
+ exact grounding keeps it zero-FP; likely audit-first.

### 6.6 Version/toolchain claims
"Requires Rust 1.75+", "Node >= 18": checkable against `rust-version` in
Cargo.toml, `engines` in package.json, `requires-python` in pyproject. Same
shape as the existing command/manifest pass, near-zero FP risk, and a very
common README staleness class.

### 6.7 HTTP route references
API docs assert routes ("`GET /api/users`") that ground against router
registrations (axum/express/flask decorators are all tree-sitter-visible).
This is the reference pass extended to a new namespace; the `module_intent`
URL exclusion in diagrams shows routes are already being recognized and
discarded — they could ground instead.

### 6.8 Deprecation claims vs. usage
"`old_client` is deprecated; use `new_client`" — checkable both ways: the
replacement symbol must exist (stale otherwise), and the deprecated symbol's
in-repo fan-in is reportable ("deprecated but still called from 14 sites") as
an advisory coverage-style finding.

### 6.9 "Did you mean" on skipped/ungrounded operands
The report tells users skipped rules aren't enforced (`report.rs:179`) but
not *why the operand failed*. For an ungrounded operand or stale diagram box,
suggest the nearest real module (edit distance / segment overlap). Turns the
most common user dead-end — a rule silently skipped after a rename — into a
one-line fix, and makes renames show up as actionable instead of invisible.

### 6.10 Rule-level fingerprints in the ledger
Claim fingerprints re-open when anchored code changes; module rules are
anchored to whole modules (`Provenance::modules`), so any edit anywhere in a
large module re-opens every rule touching it. Fingerprint edge rules on the
module *edge set* instead of module content — rules re-verify only when the
import graph actually changes, making `--diff` runs quieter and faster.

### 6.11 Doc-set hygiene
Vendored docs, translated duplicates (`README.zh.md`), and CHANGELOGs state
historical or third-party facts that shouldn't gate CI. A default skip-list
(changelogs, `vendor/`, translation suffixes) with config override removes a
whole FP-adjacent noise class — same spirit as `lang.rs`'s minified-file skip.

### 6.12 Eval-first process (keep doing it)
The existing harnesses (prose eval corpus, layer-2 recall gate, diagram
mutation harness, constswap wild audits) are the reason FP classes get caught
before shipping. Every item above should land as: fixture + eval case first,
extractor second, wild audit before promotion out of advisory.

## 7. Found while implementing section 1–3 items (third pass)

### 7.1 DOT edge chains are half-parsed
`dot.rs` matches edges with `captures()` (first match per statement), so
`a -> b -> c;` records only `a -> b` and silently drops `b -> c`. Mermaid
handles chains; DOT doesn't. Pure recall bug — iterate matches or reuse the
chain-walk logic.

### 7.2 The bare extractor doesn't skip fenced code blocks
`extract_prose_rules` skips fenced lines; `extract_bare_rules` never has. A
rule-shaped comment inside a code sample ("the handlers module must not depend
on store") can compile to a `[bare]` rule if it grounds. Audit-only today, but
it's exactly the FP class the fence skip exists for — must be fixed before any
promotion of bare rules (1.1).

### 7.3 Hedge detection needs sentence granularity ✅ (sentence split implemented)
Logical lines are now split into sentences (`sentences()` in `extract.rs`);
the hedge check + rule regexes run per sentence, so a hedge leading a *later*
sentence ("…enforced. If `api` must not import `db`, use the port.") suppresses
it, and a joined paragraph can no longer match a rule across a sentence
boundary. The whole-line hedge check is kept first so mid-line markers (`e.g.`,
`for example`) still suppress the sentence they qualify. Remaining unguarded:
a hedge appearing *mid-sentence* ("use the port abstraction if `api` must not
import `db`") — `\bif\b` anywhere would over-suppress, so this narrow case is
left as a known ceiling.

### 7.4 Duplicate extraction passes and claims (minor)
- Both rule extractors recompute `logical_lines` (and its fenced mask) over
  the same doc back-to-back in `main.rs` — each doc scanned twice.
- A constswap table row that also matches the sentence `claim_re`
  (`| port | defaults to 8080 |`) emits the same claim twice — duplicate
  identical findings, not wrong ones; dedupe on (symbol, value, origin).
- `BARE_STOPWORDS` still carries the duplicated `concretions`/`abstractions`
  entries flagged in 1.7.

### 7.5 Grounding-policy drift confirmed first-hand (reinforces 5.2)
Implementing 3.4 meant re-borrowing constswap's trust rules
(`is_code_shaped`, delimited-vs-bare value typing) by hand rather than
sharing code, and 2.3 added a fourth place (`non_real_label`) where label
semantics live. Every new extractor re-implements the same trust decisions —
the unified resolution/trust API in 5.2 is where this stops compounding.
