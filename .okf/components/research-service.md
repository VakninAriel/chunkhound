---
type: Component
title: Research Service
description: Multi-hop BFS semantic exploration with UnifiedSearch pipeline and LLM synthesis for deep codebase understanding.
tags: [component, research, llm, multi-hop, synthesis, bfs, unified-search]
timestamp: 2026-07-05T00:00:00Z
---

# Research Service

The research service answers open-ended questions about the codebase by combining
multi-hop semantic search with LLM synthesis. It is exposed as the `code_research`
MCP tool and the `research` CLI command.

**Key files:**
- `chunkhound/services/deep_research_service.py`
- `chunkhound/services/research/factory.py` — service version selector
- `chunkhound/services/research/shared/unified_search.py` — 7-step search pipeline

## Variants

| Variant | File | Strategy |
|---------|------|----------|
| v1 | `research/v1/` | Single-shot: embed query → search → LLM answer |
| v3 | `research/v3/` | Multi-hop BFS: explore → expand → synthesize |

The factory (`research/factory.py`) auto-selects based on config — v3 is default
when an LLM provider is configured.

## v3 Multi-hop BFS Flow

```
1. Initial UnifiedSearch on the root query
    ↓
2. LLM "explorer" (utility model — fast, cheap):
   "Given these chunks, what sub-questions should I investigate next?"
   → up to MAX_FOLLOWUP_QUESTIONS=3 follow-up queries
    ↓
3. UnifiedSearch on each follow-up query (BFS depth 1, MAX_DEPTH=1)
    ↓
4. Repeat steps 2–3 until:
   ├─ MAX_DEPTH reached (default 1 — shallow BFS, 1-4 total UnifiedSearch calls)
   ├─ No new sub-questions generated
   └─ Result count saturated
    ↓
5. LLM "synthesizer" (synthesis model — high-quality):
   "Given all collected chunks and sub-answers, write a comprehensive answer."
   → Markdown report with citations to source chunks
```

## UnifiedSearch — 7-Step Pipeline

Each BFS node calls `unified_search()` which itself runs a 7-step pipeline:

```
1. Multi-hop semantic search on the current query
    ↓
2. Symbol extraction: LLM identifies function/class names from results
    ↓
3. Top-N symbol selection (de-duplicated)
    ↓
4. Parallel regex search for each symbol (asyncio.gather)
    ↓
5. Unify: merge semantic results + all regex results, dedup by chunk_id
    ↓
6. Final rerank: score unified set against context.root_query
   (NOT the BFS sub-query — always the original user question)
    ↓
7. Return top results
```

**Important:** Step 6 always reranks against `context.root_query` (the original
question), not the intermediate BFS sub-query. This keeps all results ranked by
relevance to what the user actually asked, regardless of which BFS hop found them.

## BFS Call Count

With `MAX_DEPTH=1` and `MAX_FOLLOWUP_QUESTIONS=3`:
- Round 0: 1 UnifiedSearch call on the root query
- Round 1: 0–3 UnifiedSearch calls on follow-up queries

Total: **1–4 UnifiedSearch calls** per `code_research` invocation. Each
UnifiedSearch call internally runs 3 parallel searches (query expansion via
`asyncio.gather`).

## Dual-Model Architecture

- **Utility model:** fast and cheap (e.g., Claude Haiku, GPT-4o-mini) — drives exploration
- **Synthesis model:** high-quality (e.g., Claude Sonnet, GPT-4o) — writes the final report

This splits cost: many small exploration calls use the cheap model; only one
synthesis call uses the expensive one.

## HyDE (Hypothetical Document Embeddings)

Before embedding a research query, the service optionally asks the utility LLM to
generate a hypothetical code snippet that would answer the query. This hypothetical
snippet is embedded instead of the raw query text.

HyDE improves recall for natural-language queries that don't share vocabulary with
the code (e.g., "authentication flow" → generates a hypothetical `authenticate_user()`
function → embeds that → finds actual auth code).

# See Also

- [Search — Multi-hop](/algorithms/search-multi-hop.md)
- [LLM Providers](/providers/llm-providers.md)
- [MCP Server](/components/mcp-server.md)
