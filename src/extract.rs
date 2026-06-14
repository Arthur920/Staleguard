//! Pull verifiable claims out of markdown docs.
//!
//! For now this only surfaces the deterministically checkable claims (paths).
//! Layer-3 free-text claim extraction (LLM) plugs in alongside these.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone)]
pub struct PathClaim {
    /// the quoted token, e.g. "src/index.ts"
    pub raw: String,
    /// markdown file it appeared in
    pub doc_path: String,
    pub line: usize,
}

/// `backtick-quoted` tokens that look like a relative file or dir path.
fn path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"`([\w./\-]+/[\w./\-]+|[\w\-]+\.[\w]{1,5})`").unwrap())
}

/// File extensions a real on-disk path actually carries. The regex above also
/// matches dotted *code* references (`typing.Deque`, `dict.get`, `Config.extra`)
/// and prose slashes (`self/cls`, `include/exclude`, `read/write`); gating on a
/// real extension keeps those out of the path check, where they only ever
/// produced false "path does not exist" findings. Dotted symbols are the
/// symbol-resolver's job, not the filesystem's.
const PATH_EXTS: &[&str] = &[
    "py",
    "pyi",
    "pyx",
    "rs",
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "tsx",
    "go",
    "java",
    "kt",
    "rb",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "cs",
    "php",
    "swift",
    "scala",
    "clj",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "fs",
    "dart",
    "lua",
    "pl",
    "pm",
    "r",
    "jl",
    "nim",
    "zig",
    "md",
    "markdown",
    "rst",
    "adoc",
    "txt",
    "toml",
    "yaml",
    "yml",
    "json",
    "json5",
    "jsonc",
    "cfg",
    "ini",
    "conf",
    "lock",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "cmd",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "sql",
    "xml",
    "csv",
    "tsv",
    "proto",
    "graphql",
    "gql",
    "vue",
    "svelte",
    "mk",
    "gradle",
    "properties",
    "dot",
    "gv",
    "ipynb",
    "tf",
    "hcl",
    "env",
    "rake",
    "gemspec",
    "podspec",
];

/// Extensionless filenames that are nonetheless real, well-known repo files.
const PATH_FILENAMES: &[&str] = &[
    "makefile",
    "gnumakefile",
    "dockerfile",
    "license",
    "readme",
    "changelog",
    "procfile",
    "gemfile",
    "rakefile",
    "justfile",
    "vagrantfile",
    "brewfile",
];

/// Does this regex match name an actual repo path (vs. a dotted code reference or
/// a prose `a/b` slash)? True only when its final segment carries a known file
/// extension or is a known extensionless filename.
fn looks_like_path(token: &str) -> bool {
    let last = token.rsplit('/').next().unwrap_or(token);
    if let Some((_, ext)) = last.rsplit_once('.') {
        if PATH_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            return true;
        }
    }
    PATH_FILENAMES.contains(&last.to_ascii_lowercase().as_str())
}

/// Find backtick-quoted tokens that look like paths the repo should contain.
pub fn extract_path_claims(markdown: &str, doc_path: &str) -> Vec<PathClaim> {
    let mut claims = Vec::new();
    for (i, line) in markdown.lines().enumerate() {
        for cap in path_re().captures_iter(line) {
            let token = &cap[1];
            // Skip obvious non-paths (URLs, version specifiers, globs).
            if token.contains("://") || token.starts_with('*') || token.ends_with("/*") {
                continue;
            }
            if !looks_like_path(token) {
                continue;
            }
            claims.push(PathClaim {
                raw: token.to_string(),
                doc_path: doc_path.to_string(),
                line: i + 1,
            });
        }
    }
    claims
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raws(md: &str) -> Vec<String> {
        extract_path_claims(md, "README.md")
            .into_iter()
            .map(|c| c.raw)
            .collect()
    }

    #[test]
    fn real_paths_are_claimed() {
        let got = raws("See `src/main.py`, `docs/usage/custom.md`, and `pydantic-core/Makefile`.");
        assert!(got.contains(&"src/main.py".to_string()));
        assert!(got.contains(&"docs/usage/custom.md".to_string()));
        assert!(got.contains(&"pydantic-core/Makefile".to_string()));
    }

    #[test]
    fn dotted_symbols_are_not_paths() {
        // `module.Symbol` / `Type.method` refs are the symbol resolver's job;
        // they must never become "path does not exist" findings.
        let got = raws("`typing.Deque`, `dict.get`, `json.dumps`, `Config.extra`, `pydantic.v1`.");
        assert!(got.is_empty(), "dotted code refs leaked as paths: {got:?}");
    }

    #[test]
    fn prose_slashes_are_not_paths() {
        let got = raws("Either `self/cls`, `include/exclude`, or `BaseModel/RootModel`.");
        assert!(got.is_empty(), "prose slashes leaked as paths: {got:?}");
    }
}
