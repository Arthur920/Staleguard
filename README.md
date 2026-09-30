<p align="center">
  <img src="staleguard.png" alt="Staleguard logo" width="200">
</p>

# Staleguard

<p align="center">
  <a href="https://github.com/Arthur920/Staleguard/actions/workflows/ci.yml"><img src="https://github.com/Arthur920/Staleguard/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/Arthur920/Staleguard/releases"><img src="https://img.shields.io/github/v/release/Arthur920/Staleguard?sort=semver&color=blue" alt="Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/Arthur920/Staleguard?color=green" alt="License: MIT"></a>
</p>

Finds docs that lie about your code: paths, scripts, env vars, flags, and
symbols your READMEs and `CLAUDE.md` name but the repo no longer has. Local,
deterministic, tuned for TypeScript.

<p align="center">
  <img src="demo.gif" alt="Staleguard demo" width="760">
</p>

## Install

```bash
brew install Arthur920/tap/staleguard
# or: cargo install --git https://github.com/Arthur920/Staleguard
```

## Use

```bash
staleguard check
```

In CI:

```yaml
- uses: Arthur920/Staleguard@v0.4.0
```

CI baselines, SARIF, pre-commit, and config: [DETAILS.md](DETAILS.md).
