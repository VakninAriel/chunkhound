---
type: Architecture
title: Concurrency Model
description: How ChunkHound manages the database single-thread, multi-process parsing, and async embedding.
tags: [architecture, concurrency, threading, async, multiprocessing]
timestamp: 2026-06-30T00:00:00Z
---

# Concurrency Model

ChunkHound uses three different concurrency primitives for three different workloads.

## Overview

| Layer | Mechanism | Reason |
|-------|-----------|--------|
| Database | Single thread (mutex) | DuckDB is not safe for concurrent writes |
| File parsing | `ProcessPoolExecutor` | CPU-bound; avoids GIL |
| Embedding API | Async batched | I/O-bound; rate-limit aware |
| Search | Async (`asyncio`) | Non-blocking result retrieval |

## Database: Single-Threaded via SerialDatabaseProvider

**File:** `chunkhound/providers/database/serial_database_provider.py`

All database operations pass through a mutex-protected queue that serializes them
onto a single background thread. This wraps any `DatabaseProvider` implementation.

```python
provider = SerialDatabaseProvider(DuckDBProvider(db_path))
# All calls to provider are queued and executed sequentially
```

The consequence: never call database methods from multiple threads without going
through `SerialDatabaseProvider`. Direct access to the underlying `DuckDBProvider`
from async code will cause corruption.

## File Parsing: ProcessPoolExecutor

**File:** `chunkhound/services/indexing_coordinator.py`

Parsing is CPU-bound (tree-sitter grammar walking). ChunkHound uses
`ProcessPoolExecutor` with 4–16 workers (auto-scaled by file count).

**Critical:** Linux defaults multiprocessing start method to `fork`, which is unsafe
when combined with asyncio. ChunkHound forces `spawn`:

```python
import multiprocessing
multiprocessing.set_start_method("spawn", force=True)
```

Worker functions must be picklable (no lambda, no closure over non-picklable state).

## Embedding: Async Batched

**File:** `chunkhound/services/embedding_service.py`

Embedding API calls are I/O-bound. The service:
1. Fetches existing embeddings from DB (skip already-indexed chunks)
2. Batches remaining chunks into `embedding_batch_size` groups
3. Sends up to `max_concurrent_batches` requests simultaneously via `asyncio.gather`
4. Inserts results into DB in bulk (5000 per transaction)

Provider-specific concurrency limits (from `get_recommended_concurrency()`):
- OpenAI: typically 8 concurrent batches
- VoyageAI: typically 4 concurrent batches

## Realtime Indexing: Event Queue

**File:** `chunkhound/services/realtime/service.py`

File change events are buffered in a queue. The realtime service batches mutations
(100+ at a time) before handing to `IndexingCoordinator`, preventing excessive
re-embedding on rapid file saves.

## Memory and Disk Guards

Before bulk embedding operations, the service checks available system memory.
A configurable disk usage limit prevents runaway indexing from filling the disk.

# See Also

- [Indexing Coordinator](/components/indexing-coordinator.md)
- [DuckDB Provider](/providers/duckdb-provider.md)
- [Embedding Pipeline](/algorithms/embedding-pipeline.md)
- [Realtime Indexing](/components/realtime-indexing.md)
