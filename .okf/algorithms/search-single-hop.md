---
type: Algorithm
title: Search — Single-hop
description: Standard HNSW top-k vector search for semantic queries without reranking.
tags: [algorithm, search, hnsw, vector, semantic]
timestamp: 2026-06-30T00:00:00Z
---

# Single-hop Search

The default semantic search strategy. Embeds the query, runs a single HNSW
approximate nearest-neighbor lookup, and returns the top-k results.

**Key files:**
- `chunkhound/services/search/single_hop_strategy.py`
- `chunkhound/providers/database/duckdb_provider.py` — `_executor_search_semantic()`

## Flow

```
1. EmbeddingProvider.embed([query_text])
   → query_vector: list[float]  (dims = 1536 or 3072)
    ↓
2. DuckDBProvider._executor_search_semantic(query_vector, provider, model, top_k)
   SQL: SELECT chunk_id, distance FROM embeddings_{dims}
        ORDER BY array_cosine_distance(vector, ?) LIMIT top_k
    ↓
3. Fetch chunk details (symbol, code, file_path, start_line, end_line)
    ↓
4. Optional: filter by similarity threshold (distance < max_distance)
    ↓
5. Return paginated SearchResult list
```

## HNSW Index

DuckDB's native HNSW extension (`CREATE INDEX ... USING hnsw`) accelerates the
nearest-neighbor query. Without the index, the search falls back to an O(N) linear
scan.

Index characteristics:
- **Metric:** cosine distance (default)
- **Scope:** per-dimension, per-provider, per-model combination
- **Auto-rebuild:** triggered after bulk inserts (>50 embeddings) for a 12× speedup
- **Query time:** ~5ms for typical corpora

## When Single-hop Is Selected

`SearchService` picks single-hop when the embedding provider does **not** support
reranking. When reranking is available, [multi-hop search](/algorithms/search-multi-hop.md) is used instead for better recall on complex queries.

## Pagination

Results are paginated with `page_size` (default 20) and `offset`. The total count
is computed with a separate `COUNT(*)` query so callers can show "page X of Y".

# See Also

- [Search — Multi-hop](/algorithms/search-multi-hop.md)
- [Hybrid Search](/algorithms/hybrid-search.md)
- [DuckDB Provider](/providers/duckdb-provider.md)
- [Embedding Pipeline](/algorithms/embedding-pipeline.md)
