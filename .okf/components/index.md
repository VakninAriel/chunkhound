# Components

* [Indexing Coordinator](indexing-coordinator.md) - Central orchestrator: file discovery → parse → chunk diff → store
* [MCP Server](mcp-server.md) - JSON-RPC stdio/HTTP transport and tool registry
* [CLI](cli.md) - Subcommand structure, lazy imports, config wiring
* [Database Layer](database-layer.md) - DuckDB schema, HNSW index, repository pattern
* [Realtime Indexing](realtime-indexing.md) - Watchman/watchdog adapters, event batching, daemon
* [Research Service](research-service.md) - Multi-hop BFS with LLM synthesis
* [Code Mapper](code-mapper.md) - Automated documentation generation pipeline
* [DB Compaction](db-compaction.md) - Atomic compaction with HNSW bookend protocol
