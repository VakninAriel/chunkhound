---
type: Configuration
title: .chunkhound.json
description: Project-local config file format, DB path resolution, and common gotchas.
tags: [configuration, project-config, db-path, gotchas]
timestamp: 2026-06-30T00:00:00Z
---

# .chunkhound.json

A project can include a `.chunkhound.json` at its root to configure ChunkHound
without requiring CLI flags. It is the recommended approach for teams sharing a
consistent indexing configuration.

## Format

```json
{
  "database": {
    "provider": "duckdb",
    "path": ".chunkhound"
  },
  "embedding": {
    "provider": "openai",
    "model": "text-embedding-3-small"
  },
  "indexing": {
    "ignore_patterns": ["*.log", "dist/", "node_modules/"],
    "max_chunk_size": 1200
  },
  "llm": {
    "provider": "anthropic",
    "utility_model": "claude-haiku-4-5-20251001",
    "synthesis_model": "claude-sonnet-4-6"
  }
}
```

All fields are optional. Missing fields use defaults or environment variables.

## DB Path Resolution — Critical Gotchas

### Gotcha 1: `path` is relative to CWD, not the project dir

```json
{ "database": { "path": ".chunkhound" } }
```

This resolves to `<CWD>/.chunkhound`, not `<project>/.chunkhound`. When indexing
a remote project from a different working directory, always use `--db` with an
absolute path:

```bash
chunkhound index /path/to/project --db /path/to/project/.chunkhound
```

### Gotcha 2: `--config` does NOT override a local `.chunkhound.json`

If the target project has its own `.chunkhound.json`, it takes precedence over a
`--config` specified file for DB path resolution. Use explicit `--db` to override.

### Gotcha 3: Old-style flat `.chunkhound` files block directory creation

Pre-v4 ChunkHound created a flat file at `.chunkhound`. The v4+ format uses a
directory (`.chunkhound/db/chunks.db`). If the old flat file exists, DuckDB
cannot create the directory — move it aside before re-indexing:

```bash
mv .chunkhound .chunkhound.old
chunkhound index .
```

### Gotcha 4: `--db` with wrong subpath returns 0 results silently

Passing `--db /project/.chunkhound/db/chunks.db` is fine. Passing
`--db /project/.chunkhound` (the directory) is also fine — ChunkHound resolves
the `.db` file internally. But passing an incorrect path returns empty results
with no error. Always verify with a regex search:

```bash
chunkhound search --regex "." /project --db /path/to/db | head -5
```

## Default DB Path

```
<project>/.chunkhound/db/chunks.db
```

The path from `.chunkhound.json`'s `database.path` value is what you pass to
`chunkhound mcp --db`.

# See Also

- [Config System](/config/config-system.md)
- [Database Layer](/components/database-layer.md)
- [CLI](/components/cli.md)
