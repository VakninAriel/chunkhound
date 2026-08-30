---
okf_version: "0.1"
---
# ChunkHound Knowledge Bundle

ChunkHound is a semantic and regex search tool for codebases with MCP integration.
Built 100% by AI agents. This bundle captures the deep architectural and algorithmic
knowledge needed to develop ChunkHound effectively.

# Algorithms

* [cAST Chunking](/algorithms/cast-chunking.md) - AST-based semantic code chunking with greedy merge
* [Embedding Pipeline](/algorithms/embedding-pipeline.md) - Batched vectorization with caching and concurrency
* [Search: Single-hop](/algorithms/search-single-hop.md) - Standard HNSW top-k vector search
* [Search: Multi-hop](/algorithms/search-multi-hop.md) - Iterative expansion and reranking
* [Search: Regex](/algorithms/search-regex.md) - DuckDB regexp_matches with path filtering
* [Hybrid Search](/algorithms/hybrid-search.md) - Weighted combination of semantic and regex results
* [Git-diff Search](/algorithms/git-diff-search.md) - Search restricted to commit-modified files

# Architecture

* [System Overview](/architecture/overview.md) - End-to-end data flow and component map
* [Domain Models](/architecture/domain-models.md) - Chunk, File, Embedding, ChunkType, Language
* [Provider Plugin System](/architecture/provider-plugin-system.md) - Protocol-based extension points
* [Dependency Injection](/architecture/dependency-injection.md) - ProviderRegistry and factory pattern
* [Concurrency Model](/architecture/concurrency-model.md) - DB single-thread, parsers multi-process, embed async

# Components

* [Indexing Coordinator](/components/indexing-coordinator.md) - File discovery → parse → chunk diff → store
* [MCP Server](/components/mcp-server.md) - JSON-RPC stdio transport and tool registry (4 tools: search, code_research, daemon_status, websearch)
* [CLI](/components/cli.md) - Subcommand structure and config wiring
* [Database Layer](/components/database-layer.md) - DuckDB schema, HNSW, repos, WAL, per-dimension embedding tables
* [Realtime Indexing](/components/realtime-indexing.md) - Watchman/watchdog adapters and daemon
* [Research Service](/components/research-service.md) - Multi-hop BFS with UnifiedSearch 7-step pipeline and LLM synthesis
* [Code Mapper](/components/code-mapper.md) - Automated documentation pipeline
* [DB Compaction](/components/db-compaction.md) - Atomic compaction with HNSW bookend protocol

# Providers

* [DuckDB Provider](/providers/duckdb-provider.md) - Primary database backend
* [OpenAI Embeddings](/providers/openai-embeddings.md) - text-embedding-3-* with tiktoken
* [VoyageAI Embeddings](/providers/voyageai-embeddings.md) - voyage-3* with reranking
* [LLM Providers](/providers/llm-providers.md) - Anthropic, OpenAI, Gemini, CLI providers

# Configuration

* [Config System](/config/config-system.md) - Precedence order, global config file, and Pydantic hierarchy
* [.chunkhound.json](/config/dotchunkhound-json.md) - Project config format and DB path gotchas

# Operations

* [Testing](/operations/testing.md) - Mandatory smoke tests, categories, philosophy
* [Versioning](/operations/versioning.md) - hatch-vcs, tags, release automation
* [Performance](/operations/performance.md) - Batch tuning, memory limits, spawn mode
* [Rust-native Scanner](/operations/rust-native-scanner.md) - PyO3 extension for fast parallel file discovery
