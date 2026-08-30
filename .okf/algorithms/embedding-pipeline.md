---
type: Algorithm
title: Embedding Pipeline
description: Batched vectorization with skip-existing caching, async concurrency, and provider-specific limits.
tags: [algorithm, embedding, batching, caching, async]
timestamp: 2026-06-30T00:00:00Z
---

# Embedding Pipeline

ChunkHound generates vector embeddings for chunks in batches, skipping chunks that
are already embedded to avoid redundant API calls.

**Key files:**
- `chunkhound/services/embedding_service.py` — orchestration
- `chunkhound/providers/embeddings/openai_provider.py`
- `chunkhound/providers/embeddings/voyageai_provider.py`
- `chunkhound/interfaces/embedding_provider.py` — protocol

## Flow

```
Input: List[Chunk] (new or changed chunks)
    ↓
1. Fetch existing embeddings
   get_existing_embeddings(chunk_ids, provider, model)
   → Set[ChunkId] already in DB
    ↓
2. Filter to unembedded chunks
   remaining = [c for c in chunks if c.id not in existing]
    ↓
3. Batch into groups
   batch_size = provider.batch_size  (OpenAI: 2000, VoyageAI: 128)
    ↓
4. Concurrent API calls (asyncio.gather)
   max_concurrent = provider.get_recommended_concurrency()
   Each batch → provider.embed([chunk.code for chunk in batch])
    ↓
5. Bulk DB insert (5000 embeddings per transaction)
   DuckDBEmbeddingRepository.bulk_insert(embeddings)
    ↓
6. HNSW index rebuild if > 50 new embeddings added
```

## Caching — Skip Already-Embedded

`get_existing_embeddings(chunk_ids, provider, model)` queries the embeddings table
for `(chunk_id, provider, model)` tuples. Only chunks with no matching row are sent
to the API. This means:
- Re-indexing an unchanged file costs zero API calls.
- Switching providers requires re-embedding everything.
- Changing the model name (even minor version) forces re-embedding.

## Provider Interface (key subset)

```python
class EmbeddingProvider(Protocol):
    batch_size: int          # Max texts per API request
    dims: Dimensions         # Output vector dimension

    async def embed(self, texts: list[str]) -> list[list[float]]: ...
    def get_recommended_concurrency(self) -> int: ...
    def supports_reranking(self) -> bool: ...
    async def rerank(self, query, documents, top_k) -> list[RerankResult]: ...
```

## Token Safety

Before batching, each chunk's token count is estimated:
- If `tiktoken` is available: exact count via the model's tokenizer
- Fallback: `len(text) / 4` (approximate chars-per-token)

Chunks exceeding `safe_token_limit` (6000 tokens) were already split by
[cAST Chunking](/algorithms/cast-chunking.md) — the embedding pipeline treats them
as normal.

## Memory Pre-check

Before bulk operations, the service checks `psutil.virtual_memory()`. If available
memory falls below a threshold, the batch size is reduced to avoid OOM.

# See Also

- [cAST Chunking](/algorithms/cast-chunking.md)
- [OpenAI Embeddings](/providers/openai-embeddings.md)
- [VoyageAI Embeddings](/providers/voyageai-embeddings.md)
- [Concurrency Model](/architecture/concurrency-model.md)
- [Database Layer](/components/database-layer.md)
