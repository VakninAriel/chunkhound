---
type: Component
title: Indexing Coordinator
description: Central orchestrator for the file-discovery → parse → chunk-diff → embed → store pipeline.
tags: [component, indexing, parsing, pipeline, orchestration]
timestamp: 2026-06-30T00:00:00Z
---

# Indexing Coordinator

The `IndexingCoordinator` is the largest service in ChunkHound (~3300 lines) and owns
the entire bulk indexing pipeline. It coordinates file discovery, parallel parsing,
chunk diffing, embedding generation, and storage.

**File:** `chunkhound/services/indexing_coordinator.py`

## Pipeline Stages

### 1. File Discovery

```python
scan_directory_files(root_path, config)
```

- Walks the directory tree respecting `.gitignore` patterns
- Reads `.chunkhound.json` in the target directory to apply custom ignore patterns
- Returns a list of file paths to process

### 2. Parallel Parsing (ProcessPoolExecutor)

```python
batch_processor.parse_files(file_paths, worker_count)
```

- Auto-scales worker count: 4 workers for <100 files, up to 16 for large repos
- Each worker: `detect_language(path)` → `ParserFactory.create(language)` → `parser.parse(path)` → `[Chunk]`
- Workers are isolated processes (spawn start method) — one crash doesn't kill the pool
- Returns `List[ParsedFileResult]` with chunks, language, mtime, and file hash

### 3. Chunk Diffing (10× speedup on re-index)

```python
coordinator.diff_chunks(new_chunks, existing_chunks)
```

Before generating embeddings, the coordinator compares new chunks against what's
already in the database. If a chunk's `code` is identical (same hash), its existing
embedding is reused. Only changed or new chunks are sent to the embedding API.

This is why re-indexing after a small edit costs almost nothing.

### 4. Embedding Generation

Delegates to `EmbeddingService`. See [Embedding Pipeline](/algorithms/embedding-pipeline.md).

### 5. Bulk Storage

```python
db.bulk_insert_chunks(chunks, batch_size=5000)
db.bulk_insert_embeddings(embeddings, batch_size=5000)
```

- Transactions are batched at 5000 records to balance memory and commit overhead
- File records are upserted first (to get `file_id`), then chunks, then embeddings
- HNSW index is rebuilt after all embeddings are inserted (deferred for efficiency)

## Progress Reporting

The coordinator uses the `rich` library's `Progress` for hierarchical display:
- Outer bar: files processed
- Inner bars: chunks parsed, embeddings generated, records stored
- Speed: chunks/second and files/second updated in real-time

## Key Invariants

- **Upsert semantics:** if a file exists with the same hash, its chunks are not re-parsed
- **Orphan cleanup:** chunks from deleted files are removed after each index run
- **Disk guard:** if the DB grows beyond a configured size limit, indexing pauses
- **Memory guard:** embedding batches are sized down if available RAM falls below threshold

# See Also

- [cAST Chunking](/algorithms/cast-chunking.md)
- [Embedding Pipeline](/algorithms/embedding-pipeline.md)
- [Database Layer](/components/database-layer.md)
- [Concurrency Model](/architecture/concurrency-model.md)
- [Realtime Indexing](/components/realtime-indexing.md)
