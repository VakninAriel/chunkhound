---
type: Provider
title: OpenAI Embeddings
description: text-embedding-3-* provider with batch 2000, Azure support, tiktoken token counting.
tags: [provider, embedding, openai, azure, tiktoken]
timestamp: 2026-06-30T00:00:00Z
---

# OpenAI Embedding Provider

**File:** `chunkhound/providers/embeddings/openai_provider.py`

## Models

| Model | Dimensions | Use case |
|-------|-----------|---------|
| `text-embedding-3-small` | 1536 | Faster, cheaper, good quality |
| `text-embedding-3-large` | 3072 | Higher quality, higher cost |
| `text-embedding-ada-002` | 1536 | Legacy (not recommended for new projects) |

## Configuration

```json
{
  "embedding": {
    "provider": "openai",
    "model": "text-embedding-3-small",
    "api_key": "sk-...",
    "batch_size": 2000,
    "base_url": null
  }
}
```

## Key Specs

| Parameter | Value |
|-----------|-------|
| `batch_size` | 2000 texts per API request |
| `max_tokens` | 8191 tokens per text |
| `distance` | cosine |
| `supports_reranking` | Only if custom `/rerank` endpoint configured |
| `recommended_concurrency` | 8 concurrent batch requests |

## Token Counting

Uses `tiktoken` for exact token counting before batching. If `tiktoken` is not
installed, falls back to `len(text) / 4` (approximate). Chunks exceeding
`max_tokens` were already split by [cAST Chunking](/algorithms/cast-chunking.md).

## Azure OpenAI Support

Set `base_url` to your Azure endpoint and use an Azure API key. The provider
auto-detects Azure vs. standard OpenAI from the `base_url` format.

```json
{
  "embedding": {
    "provider": "openai",
    "model": "text-embedding-3-small",
    "api_key": "your-azure-key",
    "base_url": "https://your-resource.openai.azure.com/openai/deployments/your-deployment"
  }
}
```

## OpenAI-Compatible Endpoints

Setting `base_url` to a non-Azure URL enables any OpenAI-compatible server (Ollama,
local models, Fireworks, etc.). The `model` field must match the server's model name.

# See Also

- [Embedding Pipeline](/algorithms/embedding-pipeline.md)
- [VoyageAI Embeddings](/providers/voyageai-embeddings.md)
- [Config System](/config/config-system.md)
