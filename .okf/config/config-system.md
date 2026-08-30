---
type: Configuration
title: Config System
description: Pydantic-based config hierarchy with CLI > project JSON > global file > env > defaults precedence.
tags: [configuration, pydantic, precedence, env-vars, global-config]
timestamp: 2026-07-05T00:00:00Z
---

# Config System

ChunkHound's configuration is a Pydantic `BaseSettings` hierarchy that merges
sources in a defined precedence order.

**File:** `chunkhound/core/config/config.py`

## Precedence (highest → lowest)

1. **CLI arguments** — `--db`, `--model`, `--provider`, `--read-only`, etc.
2. **Local `.chunkhound.json`** — in the target directory being indexed
3. **`--config` file** — explicit config file path
4. **Environment variables** — `CHUNKHOUND_*` (double-underscore delimiter)
5. **Global config file** — machine-wide defaults (see below)
6. **Defaults** — hardcoded in the Pydantic models

A value at a higher level always wins. CLI args cannot be overridden by env vars.

**Gotcha:** `--config` does NOT override a project-local `.chunkhound.json` for DB
path. Use explicit `--db` when the target project has its own config.

## Global Config File

A machine-wide config is discovered automatically. ChunkHound checks these 6 paths
in order and uses the **first one found**:

```
1. $CHUNKHOUND_GLOBAL_CONFIG_FILE   (env var — explicit override)
2. ~/.config/chunkhound/chunkhound.json
3. ~/.config/chunkhound/config.json
4. ~/.chunkhound/config.json
5. ~/.chunkhound.json
6. /etc/chunkhound/config.json
```

Use case: store a default API key or model so all projects on the machine share it
without each project needing its own `.chunkhound.json`.

```json
{
  "embedding": {
    "provider": "voyageai",
    "api_key": "pa-..."
  },
  "llm": {
    "provider": "anthropic",
    "api_key": "sk-ant-..."
  }
}
```

## Sub-config Classes

| Class | Purpose | Key fields |
|-------|---------|-----------|
| `DatabaseConfig` | DB backend and path | `provider`, `path`, `read_only` |
| `EmbeddingConfig` | Vectorization settings | `provider`, `model`, `api_key`, `batch_size` |
| `IndexingConfig` | Parsing behavior | `max_chunk_size`, `ignore_patterns`, `workers` |
| `LLMConfig` | LLM for research/docs | `provider`, `utility_model`, `synthesis_model` |
| `ResearchConfig` | Search strategy tuning | `time_limit`, `result_limit`, `expansion_neighbors` |
| `MCPConfig` | MCP server settings | `host`, `port`, `mode` |

## Environment Variables

Environment variables use double-underscore (`__`) as a delimiter for nested config:

```bash
CHUNKHOUND_EMBEDDING__PROVIDER=openai
CHUNKHOUND_EMBEDDING__API_KEY=sk-...
CHUNKHOUND_EMBEDDING__MODEL=text-embedding-3-small
CHUNKHOUND_DATABASE__PATH=/path/to/.chunkhound
CHUNKHOUND_DATABASE__READ_ONLY=true
CHUNKHOUND_LLM__PROVIDER=anthropic
CHUNKHOUND_FRAGMENTATION_THRESHOLD_PCT=30
CHUNKHOUND_GLOBAL_CONFIG_FILE=~/.my-chunkhound-defaults.json
```

## Loading Pattern

```python
config = Config(args=parsed_args)
# Config.__init__ reads .chunkhound.json from args.path, applies precedence
errors = config.validate_for_command("search")
if errors:
    print(errors); sys.exit(1)
```

## Config Validation

Each config class implements `validate_for_command(command: str)` returning a list
of validation error messages. Commands call this before creating services, producing
user-friendly errors (e.g., "embedding provider requires OPENAI_API_KEY").

# See Also

- [.chunkhound.json](/config/dotchunkhound-json.md)
- [CLI](/components/cli.md)
- [Dependency Injection](/architecture/dependency-injection.md)
