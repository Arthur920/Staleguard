"""Planted-drift recall probe: break the code side of real doc claims, one at a
time, and check that `staleguard check` reports each break.

    python3 tools/planted_drift.py target/release/staleguard path/to/clone

Mutates the clone (then restores it with `git checkout` + `git clean`), so point
it at a throwaway clone of a repo from wild_repos.txt, never a working copy.
Prints baseline finding count (read those for false positives) and recall per
claim kind: paths (file renamed), scripts (removed from package.json), and env
vars (renamed in code).
"""
import json, os, random, re, subprocess, sys

BIN, REPO = sys.argv[1], sys.argv[2]
SKIP = {"node_modules", ".git", "dist", "build", ".next", "out", "vendor", "target"}
CODE_EXT = (".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts")
random.seed(0)


def walk(exts):
    for d, dirs, files in os.walk(REPO):
        dirs[:] = [x for x in dirs if x not in SKIP]
        for f in files:
            if f.endswith(exts):
                yield os.path.join(d, f)


def check():
    out = subprocess.run([BIN, "check", REPO, "--format", "json"], capture_output=True, text=True).stdout
    return json.loads(out)["findings"]


def restore():
    subprocess.run(["git", "-C", REPO, "checkout", "--", "."], check=True)
    subprocess.run(["git", "-C", REPO, "clean", "-fdq"], check=True)


def flagged(findings, token):
    return any(token in (f["claim"] + f["detail"]) for f in findings)


docs = {p: open(p, errors="ignore").read() for p in walk((".md", ".mdx"))}
code_files = list(walk(CODE_EXT))
code_blob = "\n".join(open(p, errors="ignore").read() for p in code_files)
ticks = {t for txt in docs.values() for t in re.findall(r"`([^`\n]{2,120})`", txt)}

# Candidate claims, taken from the docs as written.
paths = sorted(t for t in ticks
               if "/" in t and not t.startswith(("http", "/", "@")) and " " not in t
               and os.path.isfile(os.path.join(REPO, t.lstrip("./"))))
pkg = os.path.join(REPO, "package.json")
scripts = []
if os.path.isfile(pkg):
    have = json.load(open(pkg)).get("scripts", {})
    used = {m for txt in docs.values()
            for m in re.findall(r"(?:npm run|pnpm(?: run)?|yarn(?: run)?|bun run)\s+([\w:.-]+)", txt)}
    scripts = sorted(s for s in used if s in have)
envs = sorted(t for t in ticks if re.fullmatch(r"[A-Z][A-Z0-9_]{3,}", t) and t in code_blob)

baseline = check()
results = {"path": [0, 0], "script": [0, 0], "env": [0, 0]}
misses = []


def probe(kind, token):
    results[kind][1] += 1
    if flagged(check(), token):
        results[kind][0] += 1
    else:
        misses.append((kind, token))
    restore()


for p in random.sample(paths, min(5, len(paths))):
    src = os.path.join(REPO, p.lstrip("./"))
    os.rename(src, src + ".moved")
    probe("path", p)

for s in scripts[:3]:
    data = json.load(open(pkg))
    del data["scripts"][s]
    json.dump(data, open(pkg, "w"), indent=2)
    probe("script", s)

# An env var still named in CI YAML or assigned in a doc snippet stays grounded,
# so an env miss here is not necessarily a tool miss; check before trusting it.
for e in envs[:3]:
    for f in code_files:
        t = open(f, errors="ignore").read()
        if e in t:
            open(f, "w").write(re.sub(rf"\b{e}\b", e + "_RENAMED", t))
    probe("env", e)

print(json.dumps({
    "repo": os.path.basename(REPO.rstrip("/")),
    "docs": len(docs),
    "baseline_findings": len(baseline),
    "recall": {k: f"{v[0]}/{v[1]}" for k, v in results.items()},
    "misses": misses,
    "baseline_sample": [f"{f['doc_path']}: {f['detail'][:140]}" for f in baseline[:8]],
}, indent=1))
