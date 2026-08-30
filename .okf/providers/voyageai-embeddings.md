---
type: Provider
title: VoyageAI Embeddings
description: voyage-3* provider with batch 128, native reranking, and cross-encoder scoring.
tags: [provider, embedding, voyageai, reranking]
timestamp: 2026-06-30T00:00:00Z
---

# VoyageAI Embedding Provider

**File:** `chunkhound/providers/embeddings/voyageai_provider.py`

VoyageAI is the recommended provider when [multi-hop search](/algorithms/search-multi-hop.md)
quality matters, because it includes native reranking support via a cross-encoder model.

## Models

| Model | Dimensions | Notes |
|-------|-----------|-------|
| `voyage-3` | 1024 | Best quality, higher cost |
| `voyage-3-lite` | 512 | Faster, cheaper |
| `voyage-code-3` | 1024 | Code-specific training (recommended for chunkhound) |

## Key Specs

| Parameter | Value |
|-----------|-------|
| `batch_size` | 128 texts per API request |
| `supports_reranking` | ✅ Yes — enables [multi-hop search](/algorithms/search-multi-hop.md) |
| `max_rerank_batch_size` | 1000 (query + document pairs) |
| `recommended_concurrency` | 4 concurrent batch requests |
| `distance` | cosine |

## Reranking

VoyageAI's reranker uses a cross-encoder model that scores query-document relevance
more accurately than embedding cosine distance. When configured, `SearchService`
automatically selects [multi-hop search](/algorithms/search-multi-hop.md).

```python
results = await provider.rerank(
    query="authentication middleware",
    documents=[chunk.code for chunk in candidates],
    top_k=20
)
# Returns RerankResult(index, score) sorted by score descending
```

## Input Truncation

VoyageAI truncates inputs that exceed the model's token limit rather than erroring.
ChunkHound relies on [cAST Chunking](/algorithms/cast-chunking.md) to keep chunks
within the limit; the truncation is a safety net, not the primary mechanism.

# See Also

- [Embedding Pipeline](/algorithms/embedding-pipeline.md)
- [Search — Multi-hop](/algorithms/search-multi-hop.md)
- [OpenAI Embeddings](/providers/openai-embeddings.md)
