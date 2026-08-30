---
type: Component
title: Database Layer
description: DuckDB-backed storage with HNSW vector index, per-dimension embedding tables, compaction, and thread-safety wrapper.
tags: [component, database, duckdb, hnsw, repository, schema, compaction, embeddings]
timestamp: 2026-07-05T00:00:00Z
---

# Database Layer

ChunkHound uses DuckDB as its primary database. DuckDB provides both relational
(chunk/file metadata) and vector (embedding) storage in a single embedded file,
eliminating the need for a separate vector database.

**Key files:**
- `chunkhound/providers/database/duckdb_provider.py` — main provider
- `chunkhound/providers/database/duckdb/` — repository implementations
- `chunkhound/providers/database/serial_database_provider.py` — thread-safety wrapper
- `chunkhound/providers/database/compaction/` — DB compaction stages

## Schema

```sql
-- Source files
CREATE TABLE files (
    id       INTEGER PRIMARY KEY,
    path     TEXT UNIQUE NOT NULL,
    language TEXT,
    size     INTEGER,
    mtime    REAL,
    hash     TEXT,          -- SHA-256 for change detection
    created_at REAL,
    updated_at REAL
);

-- Code chunks
CREATE TABLE chunks (
    id         INTEGER PRIMARY KEY,
    file_id    INTEGER REFERENCES files(id),
    symbol     TEXT,
    code       TEXT,
    start_line INTEGER,
    end_line   INTEGER,
    chunk_type TEXT,
    language   TEXT,
    metadata   JSON
);

-- Embeddings — dimension-specific tables (e.g. embeddings_1536, embeddings_1024)
CREATE TABLE embeddings_{dims} (
    id        INTEGER PRIMARY KEY,
    chunk_id  INTEGER REFERENCES chunks(id),
    provider  TEXT,
    model     TEXT,
    vector    FLOAT[{dims}]   -- DuckDB fixed-width array type
);
CREATE UNIQUE INDEX ON embeddings_{dims} (chunk_id, provider, model);
```

### Why Per-dimension Embedding Tables?

DuckDB's `FLOAT[N]` is a **fixed-width typed array** — the dimension `N` is baked
into the column type at table creation. You cannot store a `FLOAT[1536]` and a
`FLOAT[1024]` in the same column. A separate table is created for each dimension
when a new embedding model is first used:

- `embeddings_1536` — OpenAI `text-embedding-3-small/large`
- `embeddings_1024` — VoyageAI `voyage-3`
- `embeddings_256`  — smaller/custom models

Each table has a unique index on `(chunk_id, provider, model)` so the same chunk
can be embedded by multiple providers/models without duplication.

## HNSW Vector Index

```sql
CREATE INDEX hnsw_idx ON embeddings_{dims}
USING hnsw (vector)
WITH (metric = 'cosine');
```

- Separate index per embedding table
- Built after bulk insert — not maintained incrementally
- Drop + recreate is faster than incremental updates for >50 embeddings (~12× speedup)
- Query: `ORDER BY array_cosine_distance(vector, ?) LIMIT k`

## Repository Pattern

Three repositories encapsulate SQL access:

| Repository | File | Responsibility |
|------------|------|---------------|
| `DuckDBFileRepository` | `duckdb/file_repository.py` | File CRUD, hash lookup |
| `DuckDBChunkRepository` | `duckdb/chunk_repository.py` | Chunk CRUD, bulk insert |
| `DuckDBEmbeddingRepository` | `duckdb/embedding_repository.py` | Embedding storage, HNSW queries |

## Thread Safety

DuckDB's write connection is not thread-safe. `SerialDatabaseProvider` wraps the
`DuckDBProvider` in a `ThreadPoolExecutor(max_workers=1)` that serializes all
operations through a single background thread. This prevents HNSW crashes from
concurrent access. See [Concurrency Model](/architecture/concurrency-model.md).

## WAL Mode and Checkpointing

- WAL (Write-Ahead Log) mode is enabled by default
- Automatic checkpoint at 1GB WAL size
- MVCC provides read isolation during writes

## DB Compaction

Over time, deleting chunks leaves gaps in the DuckDB file and HNSW index. Compaction
reclaims space atomically. See [DB Compaction](/components/db-compaction.md) for details.

**HNSW bookend protocol:** drop HNSW before copying data, rebuild on the compacted DB.
HNSW is in-memory and cannot be streamed across DB files.

```
had_hnsw = drop_hnsw_index()
_compact_prepare()   → create fresh empty DB
_compact_copy_data() → bulk INSERT … SELECT all live chunks + embeddings
_compact_finalize()  → atomic rename: new DB replaces old
if had_hnsw: rebuild_hnsw_index()
# on failure: _compact_restore() reverts to original
```

## Default DB Path

```
<project>/.chunkhound/db/chunks.db
```

Old-style flat `.chunkhound` files (pre-v4) block directory creation — move aside
before re-indexing if upgrading from an old version.

## Indexed Roots Sidecar

The provider maintains a sidecar file listing which root paths have been indexed,
preventing DB corruption when the same DB is used for different project directories.

# See Also

- [DB Compaction](/components/db-compaction.md)
- [DuckDB Provider](/providers/duckdb-provider.md)
- [Domain Models](/architecture/domain-models.md)
- [Concurrency Model](/architecture/concurrency-model.md)
- [.chunkhound.json](/config/dotchunkhound-json.md)
