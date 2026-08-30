---
type: Runbook
title: Versioning
description: hatch-vcs dynamic versioning from git tags, release automation, and pre-release workflow.
tags: [operations, versioning, hatch-vcs, release, pypi, git-tags]
timestamp: 2026-06-30T00:00:00Z
---

# Versioning

ChunkHound uses **dynamic versioning via hatch-vcs** — the version is derived from
git tags, not from a hardcoded string in `pyproject.toml`. Never manually edit
version strings.

## Creating a Release

```bash
# 1. Tag the version
uv run scripts/update_version.py 4.2.0

# 2. Run smoke tests (MANDATORY)
uv run pytest tests/test_smoke.py -v -n auto

# 3. Create and publish a GitHub Release
#    → triggers release.yml → PyPI upload via OIDC Trusted Publishing
```

## Pre-releases (Alpha/Beta/RC)

```bash
# Alpha
uv run scripts/update_version.py 4.2.0a1

# Beta
uv run scripts/update_version.py 4.2.0b1

# Release candidate
uv run scripts/update_version.py 4.2.0rc1
```

Pre-releases publish to **PyPI** (not TestPyPI) via `release-rc.yml` on tag push.

## Bumping Version

```bash
# Semantic bump helpers
uv run scripts/update_version.py --bump patch    # 4.1.0 → 4.1.1
uv run scripts/update_version.py --bump minor    # 4.1.0 → 4.2.0
uv run scripts/update_version.py --bump major    # 4.1.0 → 5.0.0

# Bump + pre-release suffix
uv run scripts/update_version.py --bump minor b1  # 4.1.0 → 4.2.0b1
```

## Test Release (Alpha to PyPI)

For testing the full release pipeline without a production release:

```bash
# 1. Switch remote to chunkhound org
git remote set-url origin https://github.com/chunkhound/chunkhound.git

# 2. Tag alpha
uv run scripts/update_version.py X.Y.Za1

# 3. Push tag — triggers release-rc.yml
git push origin vX.Y.Za1

# 4. Restore original remote
git remote set-url origin <original-url>
```

## CI/CD

| Workflow | Trigger | Action |
|----------|---------|--------|
| `release.yml` | GitHub Release published | Build + publish to PyPI |
| `release-rc.yml` | Tag push `v*.*.*[a/b/rc]*` | Build + publish pre-release to PyPI |
| OIDC Trusted Publishing | Both | No manual API key needed |

Do NOT use `uv publish` or `prepare_release.sh` manually — CI owns the publish step.

# See Also

- [Testing](/operations/testing.md)
