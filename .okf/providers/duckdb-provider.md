---
type: Provider
title: DuckDB Provider
description: Primary database backend — relational metadata and HNSW vector search in a single embedded file.
tags: [provider, database, duckdb, hnsw, vector]
timestamp: 2026-06-30T00:00:00Z
---

# DuckDB Provider

`DuckDBProvider` is ChunkHound's primary (and recommended) database backend. It uses
DuckDB's embedded columnar engine for both relational chunk metadata and HNSW-indexed
vector search, eliminating the need for a separate vector store.

**File:** `chunkhound/providers/database/duckdb_provider.py`

## Key Capabilities

| Capability | Implementation |
|------------|---------------|
| Relational storage | DuckDB tables (files, chunks, embeddings_N) |
| Vector similarity | `array_cosine_distance()` + HNSW index |
| Regex search | `regexp_matches()` native function |
| Bulk insert | 5000-record transactions |
| Thread safety | Wrapped in `SerialDatabaseProvider` |
| WAL mode | Enabled, checkpoint at 1GB |

## HNSW Index Lifecycle

```
Initial state: no index (linear scan for queries)
    ↓
Bulk insert of >50 embeddings
    ↓
optimize_for_bulk=True triggers:
  1. DROP INDEX IF EXISTS hnsw_idx
  2. INSERT all embeddings
  3. CREATE INDEX hnsw_idx USING hnsw(vector) WITH (metric='cosine')
    ↓
Query time: ~5ms per search (vs O(N) linear scan)
```

The drop-then-recreate pattern is intentional — incremental HNSW updates are slower
than rebuilding from scratch when adding many vectors at once.

## Optimization Flags

`optimize_for_bulk: bool` on insert methods triggers batch-optimized paths:
- Uses dimension-specific embedding tables (one table per dims value)
- Collects timing metrics for diagnostics
- Defers HNSW index rebuild until after all embeddings are inserted

## Connection Management

`DuckDBConnectionManager` handles connection lifecycle:
- Single write connection (enforced by `SerialDatabaseProvider`)
- Read connections can be multiplexed (DuckDB allows multiple readers)
- WAL checkpoint is triggered manually after large operations

## Indexed Root Sidecar

A JSON sidecar alongside the DB records which project root(s) the DB covers.
If you point `--db` at a DB that was indexed for a different project, the provider
warns rather than silently returning wrong results.

## Fragmentation and Compaction

DuckDB handles compaction internally via WAL + MVCC. There is no manual `VACUUM`
required. The `FRAGMENTATION_THRESHOLD_PCT` env var controls when the coordinator
considers the DB fragmented enough to run `CHECKPOINT` explicitly.

# See Also

- [Database Layer](/components/database-layer.md)
- [Concurrency Model](/architecture/concurrency-model.md)
- [Config System](/config/config-system.md)
- [Search — Single-hop](/algorithms/search-single-hop.md)
