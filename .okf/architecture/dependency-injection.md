---
type: Architecture
title: Dependency Injection
description: ProviderRegistry singleton and factory pattern that wires services together.
tags: [architecture, dependency-injection, registry, factory]
timestamp: 2026-06-30T00:00:00Z
---

# Dependency Injection

ChunkHound uses a lightweight registry + factory pattern rather than a full DI
framework. All providers live in a global `ProviderRegistry` singleton.

## ProviderRegistry

**File:** `chunkhound/registry/__init__.py`

```python
registry = ProviderRegistry()  # module-level singleton

def get_registry() -> ProviderRegistry:
    return registry
```

The registry stores named provider instances and exposes factory methods for services:
- `registry.get_provider("database")` → `DatabaseProvider`
- `registry.get_provider("embedding")` → `EmbeddingProvider`
- `registry.create_indexing_coordinator()` → `IndexingCoordinator`
- `registry.create_search_service()` → `SearchService`
- `registry.create_embedding_service()` → `EmbeddingService`

## Service Creation Flow

```
1. Config loaded (CLI args + env + .chunkhound.json)
        ↓
2. registry.configure(config)
   ├─ EmbeddingProviderFactory.create(config.embedding) → registered as "embedding"
   ├─ DatabaseProviderFactory.create(config.database)   → registered as "database"
   └─ LLMManager.create(config.llm)                    → registered as "llm"
        ↓
3. Command calls registry.create_<service>()
   └─ Factory pulls providers by name from registry
      → constructs service with injected dependencies
        ↓
4. Command uses service (search, index, etc.)
```

## Lazy Language Parsers

`LazyLanguageParsers` in the registry defers parser instantiation until the first
file of each language is processed — avoids loading tree-sitter grammars for unused languages.

```python
parsers = registry.get_language_parsers()
# Only python, javascript etc. parsers that are actually needed are loaded
```

## DatabaseServices Bundle

**File:** `chunkhound/database_factory.py`

Many commands need the same set of services together, so a `DatabaseServices` named
tuple bundles them:

```python
class DatabaseServices(NamedTuple):
    provider: DatabaseProvider
    indexing_coordinator: IndexingCoordinator
    search_service: SearchService
    embedding_service: EmbeddingService

services = create_services(config, db_path, embedding_provider, llm_manager)
```

This bundle is the primary entry point for CLI commands and the MCP server.

## Why Not a Full DI Framework?

The codebase is 100% AI-generated and prioritizes explicitness. The registry pattern
is straightforward to read and trace — no magic, no annotations-based injection,
no container scoping. Services are constructed imperatively and dependencies
are visible at the call site.

# See Also

- [Provider Plugin System](/architecture/provider-plugin-system.md)
- [Config System](/config/config-system.md)
- [Indexing Coordinator](/components/indexing-coordinator.md)
