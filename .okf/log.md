# ChunkHound Knowledge Bundle — Update Log

## 2026-07-05
* **Update**: `algorithms/search-multi-hop.md` — corrected initial cap (100 normal / 500 exhaustive), clarified rerank is once per expansion round (not per seed), documented dedup-before-rerank rationale, fixed "5 rerank calls" misconception.
* **Update**: `algorithms/git-diff-search.md` — added DiffAwareSearchService, Language.GIT_DIFF, ChunkType.GIT_DIFF, three search modes (db/diff/both), and in-memory diff indexing flow.
* **Update**: `components/mcp-server.md` — 3 → 4 tools (added `websearch`), documented lean markdown output format, `--read-only` flag and its `DatabaseConfig.read_only` backing.
* **Update**: `components/database-layer.md` — added per-dimension embedding table rationale (DuckDB fixed-width FLOAT[N]), compaction section, link to new db-compaction doc.
* **Update**: `components/research-service.md` — added UnifiedSearch 7-step pipeline, BFS call count (1–4), root_query vs sub-query distinction in final rerank.
* **Update**: `config/config-system.md` — added global config file with 6 auto-discovery paths, updated precedence order.
* **Creation**: `components/db-compaction.md` — HNSW bookend protocol, fragmentation monitoring, atomicity guarantee.
* **Creation**: `operations/rust-native-scanner.md` — PyO3 chunkhound_native extension, scan_files() API, fallback behavior.
* **Update**: All index files to reflect new docs and updated descriptions.

## 2026-06-30
* **Initialization**: Created foundational OKF knowledge bundle for ChunkHound.
* **Creation**: Established all six concept groups: algorithms, architecture, components, providers, config, operations.
* **Creation**: Authored 30 concept documents covering cAST chunking, HNSW search, MCP server, DuckDB layer, and all major subsystems.
