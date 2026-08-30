---
type: Component
title: Realtime Indexing
description: Filesystem watching with Watchman/watchdog adapters, event batching, and background daemon.
tags: [component, realtime, watchman, watchdog, daemon, incremental]
timestamp: 2026-06-30T00:00:00Z
---

# Realtime Indexing

ChunkHound can watch for file changes and incrementally update the index without a
full re-scan. This is the `_daemon` mode, used internally when the MCP server runs
in persistent mode.

**Key files:**
- `chunkhound/services/realtime/service.py` — main realtime service
- `chunkhound/watchman/` — Watchman session management
- `chunkhound/services/realtime/` — event pipeline

## Adapter Selection (Auto-detected)

```
default_realtime_backend_for_current_install()
→ "watchman"  if Meta's Watchman daemon is installed (optimal for large repos)
→ "watchdog"  if watchdog Python package is available (cross-platform fallback)
→ "polling"   last resort (5–10s latency, inefficient)
```

## Event Flow

```
Filesystem change detected
    ↓
Adapter emits RealtimeMutation (path, event_type: created|modified|deleted)
    ↓
RealtimePipelineMixin buffers mutations in queue
    ↓
Batch trigger: when ≥100 mutations buffered OR timeout elapsed
    ↓
IndexingCoordinator.process_batch(mutations)
  ├─ Parse changed files
  ├─ Diff chunks against existing
  ├─ Generate embeddings for new/changed chunks
  └─ Store in DB, rebuild HNSW index
    ↓
Search index updated (new content immediately queryable)
```

## Batching Strategy

The 100-mutation threshold prevents excessive re-embedding during rapid file saves
(e.g., IDE auto-save on keystroke). The batch is processed as a unit, so embeddings
for 50 changed files are generated in one API call batch rather than 50 sequential calls.

## Daemon Architecture

The `_daemon` subcommand starts a multi-client proxy that:
1. Maintains a single `IndexingCoordinator` and DB connection
2. Accepts MCP tool calls from multiple clients (e.g., multiple Claude sessions)
3. Serializes DB writes through `SerialDatabaseProvider`
4. Reports indexing status via the `daemon_status` MCP tool

## Incremental Optimization

The realtime service inherits the same chunk diffing as [Indexing Coordinator](/components/indexing-coordinator.md):
if a file's hash is unchanged (e.g., a touch without content change), the service
skips parsing and embedding entirely — zero cost.

# See Also

- [Indexing Coordinator](/components/indexing-coordinator.md)
- [MCP Server](/components/mcp-server.md)
- [Concurrency Model](/architecture/concurrency-model.md)
