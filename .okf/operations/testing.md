---
type: Runbook
title: Testing
description: Mandatory smoke tests, test categories, and the project's testing philosophy.
tags: [operations, testing, smoke-tests, pytest, philosophy]
timestamp: 2026-06-30T00:00:00Z
---

# Testing

## Mandatory Commands

```bash
# BEFORE every commit — smoke tests (~30s)
uv run pytest tests/test_smoke.py -v -n auto

# BEFORE every PR push — full suite
uv run pytest tests/ -v

# Type checking
uv run mypy chunkhound

# Linting
uv run ruff check chunkhound
```

Smoke tests are non-negotiable guardrails. They verify imports, CLI `--help`, and
server startup without requiring a running DB or API key.

## Test Categories

| Category | Location | What it covers |
|----------|----------|---------------|
| Smoke | `tests/test_smoke.py` | Module imports, CLI help, server startup |
| Unit | `tests/test_*.py` | Config parsing, provider validation, type boundaries |
| Integration | `tests/integration/` | End-to-end workflows with real DuckDB |
| E2E | `tests/e2e/` | Full system with real provider credentials |
| Config | `tests/config/` | Multi-provider config validation matrix |
| Realtime | `tests/realtime/` | File watcher behavior |
| Fixtures | `tests/fixtures/` | Reusable test repos and configs |

## Testing Philosophy

From `AGENTS.md` — these rules govern what tests are written:

1. **Test external constraints and user-facing contracts** — not internal adapters or mock behavior.
2. **Do not test** adapters, private helpers, mock behavior, or internal plumbing unless the test is the narrowest way to protect a real external contract.
3. **Name tests by contract** — `test_cli_overrides_env`, not `test_extract_cli_overrides_calls_helper`.
4. **Use real business logic** with fakes only at true external boundaries (network, filesystem, subprocess, third-party APIs).
5. **For provider integrations** — test our contract with the provider (supported/unsupported features, request validity, explicit failures). Do NOT test SDK mechanics.
6. **Before adding a test** — ask "Would a user, caller, CI contract, or external system notice if this broke?" If not, skip it.
7. **Prefer one higher-value contract test** over many narrow implementation tests.

## Running with Parallelism

```bash
# Smoke tests — always run with -n auto (parallel)
uv run pytest tests/test_smoke.py -v -n auto

# Integration tests — run sequentially if they share a DB
uv run pytest tests/integration/ -v

# All tests with timeout (Ubuntu CI uses 18min timeout for Python 3.12)
uv run pytest tests/ -v --timeout=1080
```

# See Also

- [Versioning](/operations/versioning.md)
- [Performance](/operations/performance.md)
