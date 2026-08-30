---
type: Architecture
title: Provider Plugin System
description: Protocol-based extension points for embedding, database, LLM, and search providers.
tags: [architecture, extensibility, providers, protocols]
timestamp: 2026-06-30T00:00:00Z
---

# Provider Plugin System

ChunkHound uses Python structural protocols (duck-typing interfaces) to define
extension points at every major layer. Adding a new provider means implementing
the protocol and registering it — no base class inheritance required.

## Extension Points

### Embedding Provider

**Protocol:** `chunkhound/interfaces/embedding_provider.py`

```python
class EmbeddingProvider(Protocol):
    name: str
    model: str
    dims: Dimensions
    batch_size: int
    max_tokens: int

    async def embed(self, texts: list[str]) -> list[list[float]]: ...
    def supports_reranking(self) -> bool: ...
    async def rerank(self, query: str, documents: list[str], top_k: int) -> list[RerankResult]: ...
    def get_recommended_concurrency(self) -> int: ...
```

**Registration:** Add to `EmbeddingConfig.provider` Literal + `EmbeddingProviderFactory` in `chunkhound/core/config/embedding_factory.py`.

**Existing:** `OpenAIEmbeddingProvider`, `VoyageAIEmbeddingProvider`

### Database Provider

**Protocol:** `chunkhound/interfaces/database_provider.py`

Key methods: `insert_file()`, `insert_chunk()`, `search_regex_async()`, `search_semantic()`, `create_vector_index()`, `drop_vector_index()`.

**Registration:** Add to `DatabaseConfig.provider` Literal + factory.

**Existing:** `DuckDBProvider` (primary), `LanceDBProvider` (alternative), `SerialDatabaseProvider` (thread-safety wrapper)

### LLM Provider

**Protocol:** `chunkhound/interfaces/llm_provider.py`

Used for query expansion (research service), HyDE, and code documentation synthesis.

**Registration:** Add to `LLMManager._providers` dict in `chunkhound/llm_manager.py`.

**Existing:** Anthropic, OpenAI, Gemini, Grok, OpenAI-compatible, ClaudeCode CLI, Codex CLI, OpenCode CLI

### MCP Tool

**Mechanism:** `@register_tool` decorator in `chunkhound/mcp_server/tools.py`.

```python
@register_tool(description="Search the codebase semantically", requires_embeddings=True)
async def search_semantic(query: str, path: str | None = None, page_size: int = 20) -> dict:
    ...
```

JSON Schema is auto-generated from the function signature. Tools are auto-discovered at server startup.

## How Providers Are Wired

```
Config (Pydantic)
  ↓
ProviderRegistry.configure(config)
  ├─ EmbeddingProviderFactory.create(config.embedding) → registers embedding provider
  ├─ DatabaseProviderFactory.create(config.database) → registers DB provider
  └─ LLMManager.create(config.llm) → registers LLM provider(s)
  ↓
Service factories (create_indexing_coordinator(), create_search_service(), etc.)
  └─ Pull providers from registry by name → inject into service constructors
```

## Adding a New Provider: Checklist

1. Implement the relevant Protocol class
2. Place file in `chunkhound/providers/<layer>/`
3. Add the provider name to the config Literal type
4. Register in the factory (`if config.provider == "my-provider": return MyProvider(...)`)
5. Add smoke test: import + instantiate with test config

# See Also

- [Dependency Injection](/architecture/dependency-injection.md)
- [DuckDB Provider](/providers/duckdb-provider.md)
- [OpenAI Embeddings](/providers/openai-embeddings.md)
- [LLM Providers](/providers/llm-providers.md)
