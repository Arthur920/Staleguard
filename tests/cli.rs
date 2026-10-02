//! Black-box CLI tests: run the compiled `staleguard` binary against a temporary
//! fixture repo and assert on its stdout/exit. These exercise arg parsing →
//! scan → serialization end to end, which the inline unit tests don't cover.

use std::path::PathBuf;
use std::process::Command;

/// Path to the binary cargo built for this test run.
fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_staleguard")
}

/// Create a unique temp dir with the given files (relative path, contents) and
/// return its path. Caller is responsible for cleanup via [`Fixture`].
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> Fixture {
        let dir = std::env::temp_dir().join(format!(
            "staleguard-it-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
        Fixture { dir }
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(bin())
            .args(args)
            .arg(&self.dir)
            .output()
            .expect("failed to run staleguard binary")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[test]
fn version_reports_binary_name() {
    let out = Command::new(bin()).arg("--version").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("staleguard"),
        "--version should name the binary, got: {stdout}"
    );
}

#[test]
fn stale_path_suggests_the_moved_file() {
    let fx = Fixture::new(&[
        ("pkg/helpers.rs", "pub fn greet() {}\n"),
        ("README.md", "# Demo\n\nSee `src/helpers.rs`.\n"),
    ]);
    let out = fx.run(&["check", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let details: Vec<&str> = json["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .filter_map(|f| f["detail"].as_str())
        .collect();
    assert!(
        details
            .iter()
            .any(|d| d.contains("src/helpers.rs") && d.ends_with("did you mean `pkg/helpers.rs`?")),
        "{details:?}"
    );
}

#[test]
fn check_emits_valid_sarif() {
    let fx = Fixture::new(&[
        ("lib.rs", "pub fn greet() {}\n"),
        (
            "README.md",
            "# Demo\n\nRun `staleguard frobnicate` first.\n",
        ),
    ]);
    let out = fx.run(&["check", "--format", "sarif"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let sarif: serde_json::Value =
        serde_json::from_str(&stdout).expect("sarif output is valid JSON");
    assert_eq!(sarif["version"], "2.1.0");
    assert_eq!(sarif["runs"][0]["tool"]["driver"]["name"], "Staleguard");
    // Each result must carry a physical location GitHub can annotate.
    let results = sarif["runs"][0]["results"]
        .as_array()
        .expect("results array");
    assert!(results
        .iter()
        .all(|r| r["locations"][0]["physicalLocation"]["artifactLocation"]["uri"].is_string()));
}

#[test]
fn config_excludes_doc_and_suppresses_verdict() {
    // README has a stale path; LEGACY.md has another. Exclude LEGACY.md and
    // suppress `undocumented`, so only README's stale finding remains.
    let fx = Fixture::new(&[
        ("lib.rs", "pub fn greet() {}\npub fn helper() {}\n"),
        ("README.md", "# Demo\n\nSee `src/gone.rs` for details.\n"),
        ("LEGACY.md", "# Old\n\nSee `src/also-gone.rs`.\n"),
        (
            ".staleguard.toml",
            "exclude = [\"LEGACY.md\"]\nsuppress = [\"undocumented\"]\n",
        ),
    ]);
    let out = fx.run(&["check", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("check output is valid JSON");
    let findings = json["findings"].as_array().expect("findings array");
    assert!(
        findings.iter().all(|f| f["verdict"] != "undocumented"),
        "undocumented findings should be suppressed, got: {stdout}"
    );
    assert!(
        !findings
            .iter()
            .any(|f| f["doc_path"].as_str().unwrap_or("").contains("LEGACY")),
        "excluded doc should produce no findings, got: {stdout}"
    );
}

#[test]
fn check_clean_repo_has_no_findings() {
    // A documented public symbol and nothing undocumented: no drift.
    let fx = Fixture::new(&[
        ("lib.rs", "pub fn greet() {}\n"),
        ("README.md", "# Demo\n\n`greet` greets the user.\n"),
    ]);
    let out = fx.run(&["check", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("check output is valid JSON");
    assert_eq!(
        json["findings"].as_array().map(|a| a.len()),
        Some(0),
        "clean repo should yield no findings, got: {stdout}"
    );
}

#[test]
fn docs_site_content_is_skipped_but_mdx_is_read() {
    // `www/` is an Astro docs site: its pages describe the reader's generated
    // app, so their paths aren't this repo's. Its own README and other .mdx
    // docs are still checked.
    let fx = Fixture::new(&[
        ("index.ts", "export const x = 1;\n"),
        ("www/package.json", r#"{"dependencies": {"astro": "4"}}"#),
        ("www/src/pages/start.md", "Open `src/pages/index.tsx`.\n"),
        ("www/README.md", "Theme lives in `public/theme.css`.\n"),
        ("docs/guide.mdx", "See `src/missing.ts`.\n"),
    ]);
    let out = fx.run(&["check", "--format", "json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let mut docs: Vec<&str> = json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["doc_path"].as_str().unwrap())
        .collect();
    docs.sort();
    assert_eq!(docs, ["docs/guide.mdx:1", "www/README.md:1"], "{stdout}");
}

/// Findings as `doc_path` → `detail` pairs from `--format json` output.
fn json_findings(out: &std::process::Output) -> Vec<(String, String)> {
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("valid JSON");
    json["findings"]
        .as_array()
        .expect("findings array")
        .iter()
        .map(|f| {
            (
                f["doc_path"].as_str().unwrap_or("").to_string(),
                f["detail"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

#[test]
fn diff_reports_only_drift_introduced_since_ref() {
    let fx = Fixture::new(&[
        ("lib.rs", "pub fn greet() {}\n"),
        ("scripts/deploy.sh", "echo hi\n"),
        (
            "README.md",
            "# Demo\n\nOld drift: `scripts/gone.sh`.\n\nDeploy with `scripts/deploy.sh`.\n",
        ),
    ]);
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
            .arg(&fx.dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    // A code-side change breaks a doc claim without touching the doc.
    std::fs::remove_file(fx.dir.join("scripts/deploy.sh")).unwrap();

    let out = fx.run(&["check", "--format", "json", "--diff", "HEAD"]);
    let found = json_findings(&out);
    assert!(
        found.iter().any(|(_, d)| d.contains("scripts/deploy.sh")),
        "{found:?}"
    );
    assert!(
        !found.iter().any(|(_, d)| d.contains("scripts/gone.sh")),
        "pre-existing drift should be hidden: {found:?}"
    );
    assert!(!out.status.success());
}

#[test]
fn agent_rule_files_are_checked() {
    let fx = Fixture::new(&[
        ("lib.rs", "pub fn greet() {}\n"),
        (".cursorrules", "Shared helpers live in `src/helpers.rs`.\n"),
        (".cursor/rules/api.mdc", "Routes are in `src/routes.ts`.\n"),
    ]);
    let out = fx.run(&["check", "--format", "json"]);
    let docs: Vec<String> = json_findings(&out).into_iter().map(|(p, _)| p).collect();
    assert!(
        docs.iter().any(|p| p.starts_with(".cursorrules:")),
        "{docs:?}"
    );
    assert!(
        docs.iter().any(|p| p.starts_with(".cursor/rules/api.mdc:")),
        "{docs:?}"
    );
}
