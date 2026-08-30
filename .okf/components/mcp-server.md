---
type: Component
title: MCP Server
description: JSON-RPC 2.0 stdio and HTTP transports exposing ChunkHound tools to AI assistants.
tags: [component, mcp, json-rpc, stdio, tools, websearch, read-only]
timestamp: 2026-07-05T00:00:00Z
---

# MCP Server

ChunkHound implements the Model Context Protocol (MCP) to expose search and research
capabilities to AI assistants like Claude. Two transports: stdio (default) and HTTP.

**Key files:**
- `chunkhound/mcp_server/stdio.py` — stdio JSON-RPC transport
- `chunkhound/mcp_server/tools.py` — tool registry and implementations
- `chunkhound/mcp_server/base.py` — shared lifecycle management
- `chunkhound/mcp_server/common.py` — execution harness

## !! Critical Constraint: No stdout

The stdio transport uses `stdin`/`stdout` for the JSON-RPC protocol. Any `print()`
or write to `sys.stdout` in the stdio server (including imported libraries) will
corrupt the protocol stream and break the MCP connection.

**All logging must go to stderr only** (loguru is configured this way).

## Tool Registry

Tools are registered declaratively with a decorator. JSON Schema is auto-generated
from Python type annotations — no manual schema maintenance.

```python
@register_tool(
    description="...",
    requires_embeddings=False,
    name="search",
)
async def search_impl(
    services: DatabaseServices,
    embedding_manager: EmbeddingManager | None,
    type: Literal["regex", "semantic"],
    query: str,
    path: str | None = None,
    page_size: int = 10,
    offset: int = 0,
) -> SearchResponse: ...
```

## Exposed Tools (4 total)

| Tool | Requires | Description |
|------|----------|-------------|
| `search` | embeddings (semantic only) | Unified regex/semantic search. Returns **lean markdown** format. |
| `code_research` | embeddings + LLM + reranker | Multi-hop BFS research with LLM synthesis. |
| `daemon_status` | none | Indexing health, scan progress, readiness check. |
| `websearch` | internet (DuckDuckGo) | Web search → fetch → in-memory index → deep research → markdown. |

### `search` — Lean Markdown Output

The `search` tool returns lean markdown instead of raw JSON to reduce token usage
in AI assistant contexts:

```markdown
## chunkhound/services/search/multi_hop_strategy.py L160–L220 — MultiHopStrategy.search (94%)

```python
all_results = list(initial_results)
seen_chunk_ids = {result["chunk_id"] for result in initial_results}
...
```

## chunkhound/providers/database/duckdb_provider.py L410–L450 — find_similar_chunks (87%)
...
```

Format: `## filepath L{start}–{end} — symbol (score%)` followed by fenced code block.

### `websearch` — Web Search Pipeline

```
1. Query DuckDuckGo → list of URLs
    ↓
2. Fetch and extract text from result pages (up to `limit` pages)
    ↓
3. Build in-memory ChunkHound index from fetched content
    ↓
4. Run full code_research pipeline against that in-memory index
    ↓
5. Return synthesised markdown report with citations
```

Parameters: `query` (required), `limit` (pages to fetch, default 5), `path_filter` (domain filter).

## --read-only Flag

```bash
chunkhound mcp --read-only
# or
CHUNKHOUND_DATABASE__READ_ONLY=true chunkhound mcp
```

When `--read-only` is set:
- Forces single-process stdio mode (no background writer threads)
- Sets `DatabaseConfig.read_only = True` — all DB write paths assert-fail
- The daemon watcher is not started (saves memory)
- Ideal pattern: `chunkhound index` in CI → serve frozen DB via MCP with `--read-only`

## Response Limits

To prevent context overflow in AI assistants, tool responses are capped at 20k tokens.
Paginate with `page_size` and `offset` for large result sets.

## Transports

### Stdio (default)

```bash
chunkhound mcp
```

JSON-RPC 2.0 over stdin/stdout. Used by Claude Code, Claude Desktop, and VS Code
via `.mcp.json` / `claude_desktop_config.json`.

### HTTP

```bash
chunkhound mcp http --port 5173
```

Same tools over HTTP. Useful for web-based tools or debugging with `curl`.

## MCP Client Config Examples

**Claude Code** (`.mcp.json` in project root):
```json
{
  "mcpServers": {
    "chunkhound": {
      "command": "uv",
      "args": ["run", "chunkhound", "mcp", "--db", "/path/to/.chunkhound"]
    }
  }
}
```

# See Also

- [CLI](/components/cli.md)
- [Search — Single-hop](/algorithms/search-single-hop.md)
- [Research Service](/components/research-service.md)
- [Concurrency Model](/architecture/concurrency-model.md)
