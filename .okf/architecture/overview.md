---
type: Architecture
title: System Overview
description: End-to-end data flow and component relationships in ChunkHound.
tags: [architecture, overview, data-flow]
timestamp: 2026-06-30T00:00:00Z
---

# System Overview

ChunkHound transforms source code into a searchable vector knowledge base and exposes that index through a CLI and an MCP server for AI assistant integration.

## Component Map

```
┌─────────────────────────────────────────────────────┐
│                   Entry Points                       │
│  CLI (api/cli/main.py)   MCP Server (mcp_server/)   │
└────────────┬──────────────────────┬─────────────────┘
             │                      │
             ▼                      ▼
┌────────────────────────────────────────────────────┐
│                  Service Layer                      │
│  IndexingCoordinator   SearchService               │
│  EmbeddingService      ResearchService             │
│  RealtimeIndexingService  CodeMapperService        │
└──────┬─────────────────────────┬───────────────────┘
       │                         │
       ▼                         ▼
┌──────────────────┐   ┌─────────────────────────────┐
│  Parser Layer    │   │       Provider Layer          │
│  UniversalParser │   │  DatabaseProvider (DuckDB)   │
│  ChunkSplitter   │   │  EmbeddingProvider (OpenAI)  │
│  ConceptExtract. │   │  LLMProvider (Anthropic)     │
└──────────────────┘   └─────────────────────────────┘
                                 │
                                 ▼
                        ┌────────────────┐
                        │   DuckDB DB    │
                        │  files/chunks  │
                        │  /embeddings   │
                        └────────────────┘
```

## End-to-End Indexing Flow

```
1. File Discovery
   walk_directory_tree() → gitignore-aware file list

2. Parallel Parsing (ProcessPoolExecutor, 4–16 workers)
   file → detect_language() → UniversalParser.parse() → [Chunk]

3. Chunk Diffing
   compare new chunks vs existing DB chunks
   → preserve embeddings for unchanged content (10× speedup on re-index)

4. Embedding Generation (async batched)
   fetch_existing_embeddings() → batch remaining → concurrent API calls

5. Bulk Storage (5000-record transactions)
   insert files → insert chunks → insert embeddings → rebuild HNSW index
```

## End-to-End Search Flow

```
1. Query arrives (CLI or MCP tool call)
2. SearchService selects strategy:
   ├─ Regex query → DuckDB regexp_matches()
   └─ Semantic query → EmbeddingProvider.embed(query) → HNSW top-k
3. Optional reranking (if provider supports it) → multi-hop expansion
4. ResultEnhancer: strip artifacts, add metadata, format scores
5. Return paginated results
```

## Key Invariants

- The database runs in a **single dedicated thread** (`SerialDatabaseProvider`) — DuckDB is not thread-safe for concurrent writes.
- Parsing is **multi-process** (CPU-bound) — each worker is independent and picklable.
- Embedding API calls are **async batched** (I/O-bound) — concurrent requests up to provider limit.
- **No stdout in the MCP stdio server** — breaks the JSON-RPC protocol.

# See Also

- [Domain Models](/architecture/domain-models.md)
- [Dependency Injection](/architecture/dependency-injection.md)
- [Concurrency Model](/architecture/concurrency-model.md)
- [Indexing Coordinator](/components/indexing-coordinator.md)
- [MCP Server](/components/mcp-server.md)
