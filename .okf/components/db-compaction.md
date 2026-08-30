---
type: Component
title: DB Compaction
description: Atomic database compaction that reclaims space from deleted chunks using the HNSW bookend protocol.
tags: [component, database, compaction, hnsw, duckdb, atomic]
timestamp: 2026-07-05T00:00:00Z
---

# DB Compaction

Over time, deleting chunks and re-indexing files leaves gaps in the DuckDB file.
The HNSW vector index also accumulates tombstones. Compaction reclaims that space
by building a fresh, dense copy of the DB and atomically replacing the original.

**Key files:**
- `chunkhound/providers/database/compaction/` — compaction stage implementations
- `chunkhound/providers/database/duckdb_provider.py` — triggers compaction

## Why Compaction Is Non-trivial

HNSW is an **in-memory structure** maintained by DuckDB's VSS extension. It cannot
be streamed or copied across databases — the only way to transfer it is to rebuild
it from scratch on the destination. This requires a specific protocol.

## HNSW Bookend Protocol

```
1. Check if HNSW index exists → had_hnsw = True/False
    ↓
2. DROP HNSW index (if it exists)
   Now the DB is plain relational storage — safe to copy
    ↓
3. _compact_prepare()
   Create a fresh empty DuckDB file with the same schema
    ↓
4. _compact_copy_data()
   Bulk INSERT … SELECT all live rows: files, chunks, embeddings_{dims}
   (soft-deleted or orphaned rows are excluded)
    ↓
5. _compact_finalize()
   Atomic rename: new DB file replaces the old DB file
    ↓
6. if had_hnsw: rebuild_hnsw_index()
   CREATE INDEX hnsw_idx ON embeddings_{dims} USING hnsw (vector) ...
    ↓
7. On any failure: _compact_restore()
   Revert to the original file — no data loss
```

## Fragmentation Monitoring

After each delete batch, the provider checks fragmentation:

```
fragmentation % = (total_pages - live_pages) / total_pages × 100
```

If fragmentation exceeds `CHUNKHOUND_FRAGMENTATION_THRESHOLD_PCT` (default 30%),
compaction is triggered automatically. The threshold can be tuned via env var or
config for repos with frequent re-indexing.

## Atomicity Guarantee

Step 5 (`_compact_finalize`) uses an OS-level rename. On POSIX systems this is
atomic — readers either see the old file or the new file, never a half-written
state. If the process dies between steps 4 and 5, the original DB is untouched.

## Interaction with Read-only Mode

Compaction is a write operation. If `DatabaseConfig.read_only = True`, compaction
is disabled. The `--read-only` MCP flag ensures this.

# See Also

- [Database Layer](/components/database-layer.md)
- [DuckDB Provider](/providers/duckdb-provider.md)
- [Concurrency Model](/architecture/concurrency-model.md)
