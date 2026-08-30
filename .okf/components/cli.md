---
type: Component
title: CLI
description: Command-line interface entry point with lazy-loaded subcommands and unified config wiring.
tags: [component, cli, subcommands, argparse]
timestamp: 2026-06-30T00:00:00Z
---

# CLI

ChunkHound's command-line interface is the primary way users trigger indexing and search.

**Entry point:** `chunkhound.api.cli.main:main`  
**File:** `chunkhound/api/cli/main.py`

## Subcommands

| Subcommand | Command file | Purpose |
|------------|-------------|---------|
| `index` | `commands/run.py` | Bulk index a directory |
| `search` | `commands/search.py` | Search the index |
| `mcp` | `commands/mcp.py` | Start MCP server (stdio or daemon proxy) |
| `research` | `commands/research.py` | Deep research with LLM synthesis |
| `map` | `commands/code_mapper.py` | Generate architecture documentation |
| `autodoc` | `commands/autodoc.py` | Auto-document a codebase |
| `calibrate` | `commands/calibrate.py` | Tune embedding chunk sizes |
| `_daemon` | `commands/daemon.py` | Internal multi-client daemon (not public) |

## Lazy Imports

Each command module is imported only when that subcommand is invoked. This keeps
startup time fast — heavy dependencies (DuckDB, tree-sitter, OpenAI SDK) are not
loaded unless needed.

```python
# In main.py — lazy dispatch
if args.command == "search":
    from chunkhound.api.cli.commands.search import run
    return await run(args)
```

## Config Wiring

All commands use a shared `build_config(args)` factory that reads:
1. CLI args (highest priority)
2. Environment variables (`CHUNKHOUND_*`)
3. `.chunkhound.json` in the target directory
4. Defaults

The resulting `Config` object is passed to service factories. See [Config System](/config/config-system.md).

## Key Entry Point Behaviors

- `freeze_support()` called for Windows multiprocessing compatibility
- `loguru` logging configured (stderr only — critical for MCP stdio safety)
- Async main loop started with `asyncio.run()`

## Example Invocations

```bash
# Index a project
chunkhound index /path/to/project

# Semantic search
chunkhound search "authentication middleware" /path/to/project

# Regex search
chunkhound search --regex "TODO|FIXME" /path/to/project

# Git-diff search
chunkhound search "database schema" /path/to/project --last-n 5

# Start MCP server
chunkhound mcp --db /path/to/.chunkhound
```

# See Also

- [Config System](/config/config-system.md)
- [MCP Server](/components/mcp-server.md)
- [Indexing Coordinator](/components/indexing-coordinator.md)
