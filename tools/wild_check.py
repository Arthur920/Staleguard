"""Precision regression check: run `staleguard check` on OSS repos pinned at a
commit and diff the findings against the hand-verified snapshots in tools/wild/.

    python3 tools/wild_check.py target/release/staleguard           # compare
    python3 tools/wild_check.py target/release/staleguard --update  # rewrite

A diff means a change added or lost findings: read the new ones for false
positives before running --update. Clones one repo at a time into a temp dir.
"""
import difflib, json, pathlib, subprocess, sys, tempfile

HERE = pathlib.Path(__file__).parent
BIN, UPDATE = sys.argv[1], "--update" in sys.argv


def findings(repo, sha):
    with tempfile.TemporaryDirectory() as d:
        git = ["git", "-C", d]
        subprocess.run(["git", "init", "-q", d], check=True)
        subprocess.run([*git, "fetch", "-q", "--depth", "1", f"https://github.com/{repo}", sha], check=True)
        subprocess.run([*git, "checkout", "-q", "FETCH_HEAD"], check=True)
        out = subprocess.run([BIN, "check", d, "--format", "json"], capture_output=True, text=True).stdout
    return sorted(f"{f['doc_path']}  {f['detail']}\n" for f in json.loads(out)["findings"])


failed = False
for pin in (HERE / "wild_pins.txt").read_text().split("\n"):
    if not pin.strip() or pin.startswith("#"):
        continue
    repo, sha = pin.split()
    snap = HERE / "wild" / (repo.replace("/", "__") + ".txt")
    got = findings(repo, sha)
    if UPDATE:
        snap.parent.mkdir(exist_ok=True)
        snap.write_text("".join(got))
        print(f"{repo}: wrote {len(got)} finding(s)")
        continue
    want = snap.read_text().splitlines(keepends=True) if snap.exists() else []
    diff = list(difflib.unified_diff(want, got, str(snap), "now"))
    print(f"{repo}: {len(got)} finding(s){'' if not diff else ', CHANGED'}")
    if diff:
        failed = True
        sys.stdout.writelines(diff)
sys.exit(1 if failed else 0)
