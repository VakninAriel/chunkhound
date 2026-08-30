---
type: Reference
title: Performance Tuning
description: Batch sizing, memory guards, disk limits, spawn mode, and indexing throughput optimization.
tags: [operations, performance, tuning, batch-size, memory]
timestamp: 2026-06-30T00:00:00Z
---

# Performance Tuning

## Indexing Throughput

### Parser Workers

```python
# Auto-scaled in indexing_coordinator.py
workers = min(16, max(4, file_count // 50))
```

Increase for large repos: `CHUNKHOUND_INDEXING__WORKERS=16`

### Embedding Batch Size

| Provider | Default | Max | Set via |
|----------|---------|-----|---------|
| OpenAI | 2000 | 2000 | `CHUNKHOUND_EMBEDDING__BATCH_SIZE` |
| VoyageAI | 128 | 128 | (fixed by provider) |

Larger batches = fewer API round-trips = faster indexing. Don't exceed provider limits.

### DB Insert Batch Size

5000 records per transaction (hardcoded). Changing this affects memory usage
vs. commit frequency trade-off.

### Concurrent Embedding Batches

```python
provider.get_recommended_concurrency()  # default: 8 for OpenAI, 4 for VoyageAI
```

Override: `CHUNKHOUND_EMBEDDING__MAX_CONCURRENT_BATCHES=16`

## Memory Guards

Before bulk embedding:

```python
available = psutil.virtual_memory().available
if available < MIN_FREE_BYTES:
    batch_size = batch_size // 2  # halve until safe
```

If the system is memory-constrained, batch sizes are reduced automatically.
Set `CHUNKHOUND_INDEXING__MIN_FREE_MEMORY_BYTES` to tune the threshold.

## Disk Limits

`CHUNKHOUND_DATABASE__MAX_SIZE_BYTES` — if the DuckDB file exceeds this limit,
indexing pauses and logs a warning. Default: unlimited.

## Multiprocessing Spawn Mode (Linux)

Linux defaults to `fork` for multiprocessing, which is unsafe with asyncio
(file descriptors, locks, and async state are duplicated inconsistently).
ChunkHound forces `spawn`:

```python
multiprocessing.set_start_method("spawn", force=True)
```

If you see deadlocks or hangs during indexing, verify this is set:

```python
import multiprocessing; print(multiprocessing.get_start_method())
# should print "spawn"
```

## Re-index Speed

On a second index run over an unchanged file:
- **File hash unchanged** → skip parsing entirely
- **Chunk unchanged** → skip embedding API call (reuse existing vector)
- **Net cost** → one DB read per file, near-zero for unchanged content

This 10× speedup makes `chunkhound index` safe to run on every CI pass or pre-commit hook.

## HNSW Query Performance

| Corpus size | Query time |
|-------------|-----------|
| 10k chunks | ~1ms |
| 100k chunks | ~5ms |
| 1M chunks | ~20ms |

If queries are slow, check: (1) index exists (`SELECT * FROM embedding_indexes`),
(2) DuckDB HNSW extension is loaded, (3) `SerialDatabaseProvider` is not bottlenecked
by a long-running write.

# See Also

- [Concurrency Model](/architecture/concurrency-model.md)
- [Embedding Pipeline](/algorithms/embedding-pipeline.md)
- [Database Layer](/components/database-layer.md)
- [Indexing Coordinator](/components/indexing-coordinator.md)
