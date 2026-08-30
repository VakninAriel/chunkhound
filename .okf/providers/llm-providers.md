---
type: Provider
title: LLM Providers
description: LLM backends for research synthesis, doc generation, HyDE, and query expansion.
tags: [provider, llm, anthropic, openai, gemini, hyde]
timestamp: 2026-06-30T00:00:00Z
---

# LLM Providers

LLM providers power the [Research Service](/components/research-service.md) and
[Code Mapper](/components/code-mapper.md). They are optional — core indexing and
regex/semantic search work without an LLM.

**Files:** `chunkhound/providers/llm/`, `chunkhound/llm_manager.py`

## Supported Providers

| Provider | Class | Config value |
|----------|-------|-------------|
| Anthropic | `AnthropicLLMProvider` | `"anthropic"` |
| OpenAI | `OpenAILLMProvider` | `"openai"` |
| Google Gemini | `GeminiLLMProvider` | `"gemini"` |
| Grok (XAI) | `GrokLLMProvider` | `"grok"` |
| OpenAI-compatible | `OpenAICompatibleLLMProvider` | `"openai_compatible"` |
| Claude Code CLI | `ClaudeCodeCLIProvider` | `"claude_code"` |
| Codex CLI | `CodexCLIProvider` | `"codex"` |
| OpenCode CLI | `OpenCodeCLIProvider` | `"opencode"` |

CLI providers (`claude_code`, `codex`, `opencode`) invoke the respective binary
as a subprocess — no API key required, useful for local/offline usage.

## Dual-Model Architecture

`LLMManager` maintains two separate LLM provider instances:

```python
class LLMManager:
    utility_provider: LLMProvider   # Fast, cheap — exploration, follow-ups
    synthesis_provider: LLMProvider  # High quality — final output
```

Both can be the same provider with different models, or different providers.
The [Research Service](/components/research-service.md) routes:
- BFS sub-queries → `utility_provider`
- Final synthesis → `synthesis_provider`

## HyDE (Hypothetical Document Embeddings)

HyDE uses the `utility_provider` to generate a hypothetical code snippet before
embedding a natural-language research query:

```
Query: "authentication token validation"
    ↓
utility_provider: "Write a Python function that validates authentication tokens"
    → def validate_token(token: str) -> bool: ...
    ↓
embed(hypothetical_code) instead of embed(raw_query)
    ↓
HNSW search finds actual auth code (vocabulary overlap improved)
```

HyDE is enabled automatically when an LLM provider is configured and the query
appears to be natural language rather than a code symbol.

## Configuration Example

```json
{
  "llm": {
    "provider": "anthropic",
    "utility_model": "claude-haiku-4-5-20251001",
    "synthesis_model": "claude-sonnet-4-6",
    "api_key": "sk-ant-..."
  }
}
```

# See Also

- [Research Service](/components/research-service.md)
- [Code Mapper](/components/code-mapper.md)
- [Provider Plugin System](/architecture/provider-plugin-system.md)
